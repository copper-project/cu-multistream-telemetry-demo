//! Six-camera NVENC telemetry server; no local unified logger.
use clap::Parser;
use cu_multistream_telemetry_demo::Encoding;
use cu_multistream_telemetry_demo::Robot as Server;
use cu29::prelude::*;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "192.168.1.3:7447")]
    remote: SocketAddr,
    #[arg(long, default_value = "0.0.0.0:7448")]
    bind: SocketAddr,
    /// Camera frame rate (default: 15 near-lossless/lossless, 30 CBR).
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=30))]
    fps: Option<u32>,
    /// Experimental lossless encoding with immediate encoded-buffer copying.
    #[arg(
        long = "experimental-lossless",
        alias = "lossless",
        conflicts_with = "near_lossless"
    )]
    lossless: bool,
    /// Experimental YUV444 fixed-QP encoding without the lossless encoder path.
    #[arg(long)]
    near_lossless: bool,
    /// Near-lossless quantizer: lower means higher quality and bandwidth.
    #[arg(long, default_value_t = 6, value_parser = clap::value_parser!(u32).range(1..=51))]
    qp: u32,
    #[arg(long, default_value_t = 20000000)]
    camera_bitrate: u32,
    /// Link budget in Mbit/s (default: 2000 near-lossless, 900 lossless, 200 CBR).
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=2300))]
    link_mbps: Option<u32>,
    /// Packet expiry in milliseconds (default: 800 near-lossless/lossless, 120 CBR).
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=5000))]
    latency_ms: Option<u32>,
    /// Zero runs until interrupted. Finite runs support repeatable measurements.
    #[arg(long, default_value_t = 0)]
    seconds: u64,
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let encoding = if args.near_lossless || (!args.lossless && cfg!(feature = "near-lossless")) {
        Encoding::NearLossless { qp: args.qp }
    } else if args.lossless || cfg!(feature = "lossless") {
        Encoding::Lossless
    } else {
        Encoding::Cbr {
            bitrate: args.camera_bitrate,
        }
    };
    let (default_fps, default_link_mbps, default_latency_ms) = match encoding {
        Encoding::Cbr { .. } => (30, 200, 120),
        Encoding::NearLossless { .. } => (15, 2000, 800),
        Encoding::Lossless => (15, 900, 800),
    };
    let fps = args.fps.unwrap_or(default_fps);
    let link_mbps = args.link_mbps.unwrap_or(default_link_mbps);
    let latency_ms = args.latency_ms.unwrap_or(default_latency_ms);
    let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_signal = stopped.clone();
    ctrlc::set_handler(move || stop_signal.store(true, std::sync::atomic::Ordering::Relaxed))?;
    let mut config = CuConfig::deserialize_ron(&Server::original_config())?;
    let destination = &mut config
        .log_streaming
        .as_mut()
        .expect("stream config")
        .destinations[0];
    destination.link.bitrate_bps = u64::from(link_mbps) * 1_000_000;
    destination.link.max_latency_ms = latency_ms;
    let net = config.resources[0].config.as_mut().expect("network config");
    net.set("bind_addr", args.bind.to_string());
    net.set("remote_addr", args.remote.to_string());
    for (i, device) in cu_multistream_telemetry_demo::CAMERAS.iter().enumerate() {
        let graph = config.get_graph_mut(None)?;
        let id = graph
            .get_node_id_by_name(&format!("cam{i}"))
            .expect("camera id");
        let node = graph.get_node_mut(id).expect("camera node");
        node.set_param("copy_buffers", matches!(encoding, Encoding::Lossless));
        node.set_param(
            "pipeline",
            cu_multistream_telemetry_demo::pipeline(device, fps, encoding),
        );
    }
    let mut app = Server::builder().with_config(config).build()?;
    let streams = app.copper_runtime_mut().monitor.streams.clone();
    let mut running = app.start()?;
    let started = Instant::now();
    let mut report_at = Instant::now();
    let mut last_bytes = 0;
    while !stopped.load(std::sync::atomic::Ordering::Relaxed)
        && (args.seconds == 0 || started.elapsed() < Duration::from_secs(args.seconds))
    {
        let tick_started = Instant::now();
        running.run_one_iteration()?;
        std::thread::sleep(Duration::from_micros(8333).saturating_sub(tick_started.elapsed()));
        if report_at.elapsed() >= Duration::from_secs(1) {
            if let Some(streams) = &streams {
                for stream in streams.iter() {
                    let stats = stream.snapshot();
                    let mbps = (stats.bytes_sent - last_bytes) as f64 * 8.0
                        / report_at.elapsed().as_secs_f64()
                        / 1e6;
                    last_bytes = stats.bytes_sent;
                    let feedback = stats.feedback.unwrap_or_default();
                    eprintln!(
                        "{mbps:.1} Mbit/s tx_drops={} expired={} inbox_drops={} feedback={:?} reports={} loss={:.2}% repair_every={}",
                        stats.transport_drops,
                        stats.expired_packets,
                        stats.inbox_drops,
                        feedback.state,
                        feedback.reports,
                        f64::from(feedback.loss_basis_points) / 100.0,
                        feedback.effective_repair_every_source_symbols
                    );
                }
            }
            report_at = Instant::now();
        }
    }
    drop(running.stop()?);
    Ok(())
}
