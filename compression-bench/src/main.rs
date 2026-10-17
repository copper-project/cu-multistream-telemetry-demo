//! Benchmark independent lossless frames, including reversible UYVY preprocessing.
use clap::Parser;
use std::fs::File;
use std::hint::black_box;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::time::Instant;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Parser)]
struct Args {
    /// Tightly packed UYVY files, one per camera. No color conversion is applied.
    #[arg(required = true)]
    files: Vec<PathBuf>,
    #[arg(long, default_value_t = 1920)]
    width: usize,
    #[arg(long, default_value_t = 1080)]
    height: usize,
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..))]
    frames: u32,
    #[arg(long, default_value_t = 10)]
    skip: u32,
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(1..))]
    repeats: u32,
    #[arg(long, default_value_t = 15, value_parser = clap::value_parser!(u32).range(1..))]
    fps: u32,
    /// Budget for compressed payload; leave room for Ethernet, Copper, and FEC.
    #[arg(long, default_value_t = 2200)]
    budget_mbps: u32,
}

#[derive(Clone, Copy, Debug)]
enum Layout {
    Packed,
    Planar,
    Delta,
}

// Deinterleave UYVY into Y/U/V. Delta subtracts the left sample within each row.
fn prepare(input: &[u8], output: &mut [u8], width: usize, layout: Layout) {
    let pixels = input.len() / 2;
    for (pair, uyvy) in input.as_chunks::<4>().0.iter().enumerate() {
        output[pair * 2] = uyvy[1];
        output[pair * 2 + 1] = uyvy[3];
        output[pixels + pair] = uyvy[0];
        output[pixels + pixels / 2 + pair] = uyvy[2];
    }
    if matches!(layout, Layout::Delta) {
        let (y, chroma) = output.split_at_mut(pixels);
        let (u, v) = chroma.split_at_mut(pixels / 2);
        for (plane, stride) in [(y, width), (u, width / 2), (v, width / 2)] {
            for row in plane.chunks_exact_mut(stride) {
                for x in (1..stride).rev() {
                    row[x] = row[x].wrapping_sub(row[x - 1]);
                }
            }
        }
    }
}

fn restore(input: &mut [u8], output: &mut [u8], width: usize, layout: Layout) {
    if matches!(layout, Layout::Packed) {
        output.copy_from_slice(input);
        return;
    }
    let pixels = input.len() / 2;
    if matches!(layout, Layout::Delta) {
        let (y, chroma) = input.split_at_mut(pixels);
        let (u, v) = chroma.split_at_mut(pixels / 2);
        for (plane, stride) in [(y, width), (u, width / 2), (v, width / 2)] {
            for row in plane.chunks_exact_mut(stride) {
                for x in 1..stride {
                    row[x] = row[x].wrapping_add(row[x - 1]);
                }
            }
        }
    }
    for (pair, uyvy) in output.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        uyvy[0] = input[pixels + pair];
        uyvy[1] = input[pair * 2];
        uyvy[2] = input[pixels + pixels / 2 + pair];
        uyvy[3] = input[pair * 2 + 1];
    }
}

#[derive(Clone, Copy, Debug)]
enum Codec {
    Lz4,
    Zstd(i32),
}

struct Encoder {
    codec: Codec,
    zstd: zstd::bulk::Compressor<'static>,
}

impl Encoder {
    fn new(codec: Codec) -> Result<Self> {
        Ok(Self {
            codec,
            zstd: zstd::bulk::Compressor::new(match codec {
                Codec::Lz4 => 1,
                Codec::Zstd(level) => level,
            })?,
        })
    }

    fn compress(&mut self, input: &[u8], output: &mut [u8]) -> Result<usize> {
        Ok(match self.codec {
            Codec::Lz4 => lz4::block::compress_to_buffer(input, None, false, output)?,
            Codec::Zstd(_) => self.zstd.compress_to_buffer(input, output)?,
        })
    }
}

struct Samples {
    raw: Arc<Vec<Vec<u8>>>,
    compressed: Vec<Vec<u8>>,
}

struct Worker {
    encoder: Encoder,
    decoder: zstd::bulk::Decompressor<'static>,
    prepared: Vec<u8>,
    output: Vec<u8>,
    restored: Vec<u8>,
}

impl Worker {
    fn new(codec: Codec, frame_bytes: usize) -> Result<Self> {
        Ok(Self {
            encoder: Encoder::new(codec)?,
            decoder: zstd::bulk::Decompressor::new()?,
            prepared: vec![0; frame_bytes],
            output: vec![
                0;
                lz4::block::compress_bound(frame_bytes)?
                    .max(zstd::zstd_safe::compress_bound(frame_bytes))
            ],
            restored: vec![0; frame_bytes],
        })
    }

    fn encode(&mut self, raw: &[u8], width: usize, layout: Layout) -> Result<usize> {
        let input = if matches!(layout, Layout::Packed) {
            raw
        } else {
            prepare(raw, &mut self.prepared, width, layout);
            &self.prepared
        };
        self.encoder.compress(input, &mut self.output)
    }

    fn decode(&mut self, compressed: &[u8], width: usize, layout: Layout) -> Result<()> {
        let size = match self.encoder.codec {
            Codec::Lz4 => lz4::block::decompress_to_buffer(
                compressed,
                Some(self.prepared.len().try_into()?),
                &mut self.prepared,
            )?,
            Codec::Zstd(_) => self
                .decoder
                .decompress_to_buffer(compressed, &mut self.prepared[..])?,
        };
        if size != self.prepared.len() {
            return Err("decoded frame size mismatch".into());
        }
        restore(&mut self.prepared, &mut self.restored, width, layout);
        Ok(())
    }
}

// A barrier aligns worker starts; setup, file I/O, and verification are untimed.
fn measure(
    samples: &[Arc<Samples>],
    args: &Args,
    codec: Codec,
    layout: Layout,
    workers: usize,
    decode: bool,
) -> Result<(f64, Vec<f64>)> {
    let barrier = Arc::new(Barrier::new(workers + 1));
    let mut states = (0..workers)
        .map(|_| Worker::new(codec, args.width * args.height * 2))
        .collect::<Result<Vec<_>>>()?;
    for (index, worker) in states.iter_mut().enumerate() {
        let first = &samples[index % samples.len()];
        worker.encode(&first.raw[0], args.width, layout)?;
        worker.decode(&first.compressed[0], args.width, layout)?;
    }
    std::thread::scope(|scope| {
        let handles: Vec<_> = states
            .into_iter()
            .enumerate()
            .map(|(index, mut worker)| {
                let barrier = barrier.clone();
                scope.spawn(move || -> Result<Vec<f64>> {
                    let mut timings = Vec::with_capacity(
                        args.repeats as usize
                            * args.frames as usize
                            * samples.len().div_ceil(workers),
                    );
                    barrier.wait();
                    for _ in 0..args.repeats {
                        for sample in samples.iter().skip(index).step_by(workers) {
                            for (raw, compressed) in sample.raw.iter().zip(&sample.compressed) {
                                let start = Instant::now();
                                if decode {
                                    worker.decode(compressed, args.width, layout)?;
                                    black_box(&worker.restored);
                                } else {
                                    let size = worker.encode(raw, args.width, layout)?;
                                    black_box(&worker.output[..size]);
                                }
                                timings.push(start.elapsed().as_secs_f64());
                            }
                        }
                    }
                    Ok(timings)
                })
            })
            .collect();
        let start = Instant::now();
        barrier.wait();
        let mut timings = Vec::new();
        for handle in handles {
            timings.extend(handle.join().map_err(|_| "benchmark worker panicked")??);
        }
        Ok((start.elapsed().as_secs_f64(), timings))
    })
}

fn percentile(timings: &mut [f64], fraction: f64) -> f64 {
    timings.sort_unstable_by(f64::total_cmp);
    timings[((timings.len() - 1) as f64 * fraction).ceil() as usize] * 1000.0
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.width == 0 || !args.width.is_multiple_of(2) || args.height == 0 {
        return Err("UYVY requires positive dimensions and an even width".into());
    }
    let bytes = args
        .width
        .checked_mul(args.height)
        .and_then(|n| n.checked_mul(2))
        .filter(|n| *n <= i32::MAX as usize)
        .ok_or("frame too large")?;
    let mut raw_samples = Vec::new();
    for path in &args.files {
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(bytes as u64 * u64::from(args.skip)))?;
        let frames = (0..args.frames)
            .map(|_| {
                let mut frame = vec![0; bytes];
                file.read_exact(&mut frame)?;
                Ok(frame)
            })
            .collect::<Result<Vec<_>>>()?;
        raw_samples.push(frames);
    }
    let raw_samples: Vec<_> = raw_samples.into_iter().map(Arc::new).collect();
    let frame_count = args.frames as usize * args.files.len();
    let raw_mbps = bytes as f64 * args.fps as f64 * args.files.len() as f64 * 8.0 / 1e6;
    eprintln!(
        "{} cameras, {}x{}, {} frames/camera, {} repeats, target {} fps/camera, raw {:.1} Mbit/s, payload budget {} Mbit/s",
        args.files.len(),
        args.width,
        args.height,
        args.frames,
        args.repeats,
        args.fps,
        raw_mbps,
        args.budget_mbps
    );
    println!(
        "layout,codec,workers,ratio,payload_mbps,max_frame_bytes,encode_MB_s,encode_fps_per_camera,encode_p95_ms,decode_MB_s,decode_fps_per_camera,decode_p95_ms,encode_realtime,payload_fits"
    );
    for layout in [Layout::Packed, Layout::Planar, Layout::Delta] {
        for codec in [Codec::Lz4, Codec::Zstd(-3), Codec::Zstd(1), Codec::Zstd(3)] {
            let mut samples = Vec::new();
            let mut compressed_bytes = 0usize;
            let mut max_frame_bytes = 0;
            let mut worker = Worker::new(codec, bytes)?;
            for raw in &raw_samples {
                let mut compressed = Vec::new();
                for frame in raw.iter() {
                    let n = worker.encode(frame, args.width, layout)?;
                    let block = worker.output[..n].to_vec();
                    worker.decode(&block, args.width, layout)?;
                    if worker.restored != *frame {
                        return Err(format!("roundtrip mismatch: {layout:?}/{codec:?}").into());
                    }
                    compressed_bytes += n;
                    max_frame_bytes = max_frame_bytes.max(n);
                    compressed.push(block);
                }
                samples.push(Arc::new(Samples {
                    raw: raw.clone(),
                    compressed,
                }));
            }
            let ratio = (bytes * frame_count) as f64 / compressed_bytes as f64;
            let mbps = raw_mbps / ratio;
            for workers in [1, samples.len()] {
                let (encode_s, mut enc) = measure(&samples, &args, codec, layout, workers, false)?;
                let (decode_s, mut dec) = measure(&samples, &args, codec, layout, workers, true)?;
                let frames = frame_count as f64 * args.repeats as f64;
                let enc_fps = frames / encode_s / samples.len() as f64;
                println!(
                    "{layout:?},{codec:?},{workers},{ratio:.4},{mbps:.1},{max_frame_bytes},{:.1},{enc_fps:.2},{:.3},{:.1},{:.2},{:.3},{},{}",
                    bytes as f64 * frames / encode_s / 1e6,
                    percentile(&mut enc, 0.95),
                    bytes as f64 * frames / decode_s / 1e6,
                    frames / decode_s / samples.len() as f64,
                    percentile(&mut dec, 0.95),
                    enc_fps >= args.fps as f64,
                    mbps <= args.budget_mbps as f64
                );
                if samples.len() == 1 {
                    break;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transforms_and_codecs_preserve_every_byte() -> Result<()> {
        // Exercise row boundaries, chroma placement, and wrapping subtraction.
        let raw: Vec<_> = (0..256).map(|n| ((n * 197 + 31) % 256) as u8).collect();
        for layout in [Layout::Packed, Layout::Planar, Layout::Delta] {
            for codec in [Codec::Lz4, Codec::Zstd(-3), Codec::Zstd(1), Codec::Zstd(3)] {
                let mut worker = Worker::new(codec, raw.len())?;
                let n = worker.encode(&raw, 16, layout)?;
                let compressed = worker.output[..n].to_vec();
                worker.decode(&compressed, 16, layout)?;
                assert_eq!(worker.restored, raw, "{layout:?}/{codec:?}");
            }
        }
        Ok(())
    }
}
