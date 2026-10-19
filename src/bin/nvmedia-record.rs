//! Record six lossless cameras in a local Copper log, or extract its H.265 streams.
use clap::{Parser, Subcommand};
use cu29::prelude::*;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[copper_runtime(config = "copperconfig-record.ron")]
struct Recorder {}

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Capture six cameras without starting the UDP transport.
    Record {
        /// Fresh Copper log base path; numbered slabs are stored beside it.
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value_t = 30)]
        seconds: u64,
        #[arg(long, default_value_t = 15, value_parser = clap::value_parser!(u32).range(1..=30))]
        fps: u32,
    },
    /// Extract complete access units from the recorded CopperLists for decoding.
    Extract {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Args::parse().command {
        Command::Record {
            output,
            seconds,
            fps,
        } => {
            let parent = output
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(std::path::Path::new("."));
            let stem = output
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or("Invalid log filename")?;
            let slab_prefix = format!("{stem}_");
            let existing_slabs = if parent.exists() {
                std::fs::read_dir(parent)?
                    .collect::<Result<Vec<_>, _>>()?
                    .iter()
                    .any(|entry| {
                        let name = entry.file_name();
                        let name = name.to_string_lossy();
                        name.starts_with(&slab_prefix) && name.ends_with(".copper")
                    })
            } else {
                false
            };
            if output.symlink_metadata().is_ok() || existing_slabs {
                return Err("Output already exists; choose a fresh log path".into());
            }
            if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            let stopped = Arc::new(AtomicBool::new(false));
            let signal = stopped.clone();
            ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
            let mut config = CuConfig::deserialize_ron(&default::Recorder::original_config())?;
            for (index, device) in cu_multistream_telemetry_demo::CAMERAS.iter().enumerate() {
                let graph = config.get_graph_mut(None)?;
                let id = graph
                    .get_node_id_by_name(&format!("cam{index}"))
                    .ok_or("Missing camera ID")?;
                let node = graph.get_node_mut(id).ok_or("Missing camera node")?;
                node.set_param(
                    "pipeline",
                    cu_multistream_telemetry_demo::nvmedia::capture_pipeline(device, fps),
                );
                node.set_param("fps", fps);
            }
            let app = default::Recorder::builder()
                .with_config(config)
                .with_log_path(&output, Some(1024 * 1024 * 1024))?
                .build()?;
            let mut running = app.start()?;
            let start = Instant::now();
            let result = (|| -> CuResult<()> {
                while !stopped.load(Ordering::Relaxed)
                    && (seconds == 0 || start.elapsed() < Duration::from_secs(seconds))
                {
                    let tick = Instant::now();
                    running.run_one_iteration()?;
                    std::thread::sleep(Duration::from_micros(8333).saturating_sub(tick.elapsed()));
                }
                Ok(())
            })();
            let recorded_seconds = start.elapsed().as_secs_f64();
            let shutdown = Instant::now();
            drop(running.stop()?);
            result?;
            println!(
                "recorded_seconds={recorded_seconds:.3} shutdown_seconds={:.3} output={}",
                shutdown.elapsed().as_secs_f64(),
                output.display()
            );
        }
        Command::Extract { input, output } => {
            std::fs::create_dir(&output)?;
            let mut files = (0..6)
                .map(|i| std::fs::File::create_new(output.join(format!("cam{i}.h265"))))
                .collect::<Result<Vec<_>, _>>()?;
            let mut counts = [0u64; 6];
            let mut bytes = [0u64; 6];
            let mut lists = 0;
            let log = UnifiedLoggerRead::new(&input)?;
            let mut reader = std::io::BufReader::new(memmap::UnifiedLoggerIOReader::new(
                log,
                UnifiedLogType::CopperList,
            ));
            loop {
                // Peek before decoding so a truncated record is an error, not silent EOF.
                if std::io::BufRead::fill_buf(&mut reader)?.is_empty() {
                    break;
                }
                let list: CopperList<default::CuStampedDataSet> =
                    cu29::bincode::decode_from_std_read(
                        &mut reader,
                        cu29::bincode::config::standard().with_limit::<134217728>(),
                    )?;
                lists += 1;
                let frames = [
                    list.msgs.get_cam_0_output(),
                    list.msgs.get_cam_1_output(),
                    list.msgs.get_cam_2_output(),
                    list.msgs.get_cam_3_output(),
                    list.msgs.get_cam_4_output(),
                    list.msgs.get_cam_5_output(),
                ];
                for (index, frame) in frames.iter().enumerate() {
                    if let Some(payload) = frame.payload() {
                        let buffer = payload.map_readable()?;
                        files[index].write_all(buffer.as_slice())?;
                        counts[index] += 1;
                        bytes[index] += buffer.len() as u64;
                    }
                }
            }
            println!("copperlists={lists} frames={counts:?} bytes={bytes:?}");
            if counts.contains(&0) {
                return Err("One or more cameras recorded no frames".into());
            }
        }
    }
    Ok(())
}
