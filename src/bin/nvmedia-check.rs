//! Record synthetic or camera frames locally for byte-exact software decode checks.
use clap::Parser;
use cu_multistream_telemetry_demo::nvmedia::{Encoder, capture_pipeline, sample_to_y444};
use gstreamer::prelude::*;
use gstreamer_video::VideoFrameExt;
use std::io::Write;
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    /// Fresh directory for reference.y444, upload.y444, stream.h265, and frame sizes.
    #[arg(long)]
    output: PathBuf,
    /// Omit to encode full-entropy synthetic YUV444, including values 0 and 255.
    #[arg(long)]
    camera: Option<String>,
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=10000))]
    frames: u32,
    #[arg(long, default_value_t = 15, value_parser = clap::value_parser!(u32).range(1..=30))]
    fps: u32,
    /// Measure queued camera encoding without CPU reference maps, readback, or disk output.
    #[arg(long, requires = "camera")]
    benchmark: bool,
}
struct Pipeline(Option<gstreamer::Pipeline>);
impl Drop for Pipeline {
    fn drop(&mut self) {
        if let Some(pipeline) = &self.0 {
            let _ = pipeline.set_state(gstreamer::State::Null);
        }
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if args.benchmark {
        return benchmark(&args);
    }
    std::fs::create_dir(&args.output)?;
    let mut reference = std::fs::File::create_new(args.output.join("reference.y444"))?;
    let mut upload = std::fs::File::create_new(args.output.join("upload.y444"))?;
    let mut stream = std::fs::File::create_new(args.output.join("stream.h265"))?;
    let mut sizes = std::fs::File::create_new(args.output.join("frames.csv"))?;
    writeln!(sizes, "frame,encoded_bytes,upload_us,encode_us,retrieve_us")?;
    let mut native = if args.camera.is_some() {
        Some(std::io::BufWriter::new(std::fs::File::create_new(
            args.output.join("reference.uyvy"),
        )?))
    } else {
        None
    };
    let started = std::time::Instant::now();
    let mut pipeline = Pipeline(None);
    let sink = if let Some(device) = &args.camera {
        gstreamer::init()?;
        let capture = gstreamer::parse::launch(&capture_pipeline(device, args.fps))?
            .downcast::<gstreamer::Pipeline>()
            .map_err(|_| "Expected pipeline")?;
        let sink = capture
            .by_name("copper")
            .ok_or("Missing appsink")?
            .downcast::<gstreamer_app::AppSink>()
            .map_err(|_| "Expected appsink")?;
        capture.set_state(gstreamer::State::Playing)?;
        pipeline.0 = Some(capture);
        Some(sink)
    } else {
        None
    };
    let mut encoder = Encoder::new(args.fps)?;
    let mut frame = vec![0; 1920 * 1080 * 3];
    let mut readback = frame.clone();
    let mut encoded = std::sync::Arc::<[u8]>::default();
    let mut pending = std::collections::VecDeque::new();
    let mut random = 0x123456789abcdef0u64;
    for index in 0..args.frames {
        encoder.reclaim_inputs()?;
        let mut captured = None;
        if let Some(sink) = &sink {
            let sample = sink
                .try_pull_sample(gstreamer::ClockTime::from_seconds(10))
                .ok_or("Camera frame timed out")?;
            sample_to_y444(&sample, &mut frame)?;
            if let Some(native) = &mut native {
                let info =
                    gstreamer_video::VideoInfo::from_caps(sample.caps().ok_or("Missing caps")?)?;
                let raw = gstreamer_video::VideoFrameRef::from_buffer_ref_readable(
                    sample.buffer().ok_or("Missing buffer")?,
                    &info,
                )?;
                let stride = usize::try_from(raw.plane_stride()[0])?;
                let data = raw.plane_data(0)?;
                for row in 0..1080 {
                    native.write_all(&data[row * stride..row * stride + 3840])?;
                }
            }
            captured = Some(sample);
        } else {
            for chunk in frame.as_chunks_mut::<8>().0 {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                chunk.copy_from_slice(&random.to_le_bytes());
            }
        }
        reference.write_all(&frame)?;
        if let Some(sample) = &captured {
            encoder.submit_sample(sample)?;
            pending.push_back((index, frame.clone()));
            if encoder.queued() < encoder.capacity() {
                continue;
            }
            encoder.receive(&mut encoded)?;
        } else {
            encoder.encode(&mut frame, &mut encoded)?;
            pending.push_back((index, frame.clone()));
        }
        let (index, expected) = pending.pop_front().ok_or("Missing reference frame")?;
        encoder.read_input(&mut readback)?;
        if expected != readback {
            return Err("NvSciBuf upload changed input bytes".into());
        }
        upload.write_all(&readback)?;
        stream.write_all(&encoded)?;
        let timing = encoder.last_timings();
        writeln!(
            sizes,
            "{index},{},{},{},{}",
            encoded.len(),
            timing.upload.as_micros(),
            timing.encode.as_micros(),
            timing.retrieve.as_micros()
        )?;
    }
    while let Some((index, expected)) = pending.pop_front() {
        encoder.receive(&mut encoded)?;
        encoder.read_input(&mut readback)?;
        if expected != readback {
            return Err("Queued NvSciBuf conversion changed bytes".into());
        }
        upload.write_all(&readback)?;
        stream.write_all(&encoded)?;
        let timing = encoder.last_timings();
        writeln!(
            sizes,
            "{index},{},{},{},{}",
            encoded.len(),
            timing.upload.as_micros(),
            timing.encode.as_micros(),
            timing.retrieve.as_micros()
        )?;
    }
    if let Some(native) = &mut native {
        native.flush()?;
    }
    println!(
        "recorded_frames={} upload_exact=true elapsed_seconds={:.3} output={}",
        args.frames,
        started.elapsed().as_secs_f64(),
        args.output.display()
    );
    Ok(())
}

fn benchmark(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir(&args.output)?;
    gstreamer::init()?;
    let camera = args.camera.as_ref().ok_or("Benchmark requires --camera")?;
    let capture = gstreamer::parse::launch(&capture_pipeline(camera, args.fps))?
        .downcast::<gstreamer::Pipeline>()
        .map_err(|_| "Expected pipeline")?;
    let pipeline = Pipeline(Some(capture));
    let capture = pipeline.0.as_ref().ok_or("Missing pipeline")?;
    let sink = capture
        .by_name("copper")
        .ok_or("Missing appsink")?
        .downcast::<gstreamer_app::AppSink>()
        .map_err(|_| "Expected appsink")?;
    let mut encoder = Encoder::new(args.fps)?;
    let mut sizes = std::fs::File::create_new(args.output.join("frames.csv"))?;
    writeln!(sizes, "frame,encoded_bytes,upload_us,encode_us,retrieve_us")?;
    capture.set_state(gstreamer::State::Playing)?;
    let mut first = None;
    let mut received = 0u32;
    let mut bytes = 0u64;
    let mut encoded = std::sync::Arc::<[u8]>::default();
    for index in 0..args.frames {
        encoder.reclaim_inputs()?;
        let sample = sink
            .try_pull_sample(gstreamer::ClockTime::from_seconds(10))
            .ok_or("Camera frame timed out")?;
        encoder.submit_sample(&sample)?;
        if encoder.queued() < encoder.capacity() && index + 1 < args.frames {
            continue;
        }
        encoder.receive(&mut encoded)?;
        first.get_or_insert_with(std::time::Instant::now);
        bytes += encoded.len() as u64;
        let timing = encoder.last_timings();
        writeln!(
            sizes,
            "{received},{},{},{},{}",
            encoded.len(),
            timing.upload.as_micros(),
            timing.encode.as_micros(),
            timing.retrieve.as_micros()
        )?;
        received += 1;
    }
    while encoder.queued() > 0 {
        encoder.receive(&mut encoded)?;
        bytes += encoded.len() as u64;
        let timing = encoder.last_timings();
        writeln!(
            sizes,
            "{received},{},{},{},{}",
            encoded.len(),
            timing.upload.as_micros(),
            timing.encode.as_micros(),
            timing.retrieve.as_micros()
        )?;
        received += 1;
    }
    let elapsed = first.ok_or("No encoded frames")?.elapsed().as_secs_f64();
    println!(
        "camera={camera} frames={received} elapsed_seconds={elapsed:.3} fps={:.3} bytes={bytes} raw_cpu_maps=0",
        received.saturating_sub(1) as f64 / elapsed
    );
    Ok(())
}
