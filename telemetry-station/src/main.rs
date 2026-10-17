//! Typed Copper twin with six bounded, asynchronous HEVC decoders and an egui view.
use clap::Parser;
use cu_multistream_telemetry_demo::twin::Twin;
use cu29_logstream::{CuStreamRx, CuStreamRxError, CuTwin, CuTwinStatus, SessionRouterLimits};
use cu29_logstream_udp::{CuUdpLogStreamConfig, CuUdpLogStreamRx};
use eframe::egui;
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app::{AppSink, AppSinkCallbacks, AppSrc};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "0.0.0.0:7447")]
    listen: SocketAddr,
    #[arg(long, default_value = "192.168.1.145:7448")]
    feedback_to: SocketAddr,
    /// Copper log base path. Default: logs/telemetry-<timestamp>.copper.
    #[arg(long)]
    archive: Option<PathBuf>,
    #[arg(long)]
    headless: bool,
    #[arg(long, default_value_t = 0)]
    seconds: u64,
}

#[derive(Debug)]
struct CountedRx {
    rx: CuUdpLogStreamRx,
    bytes: Arc<AtomicU64>,
}
impl CuStreamRx for CountedRx {
    fn try_recv(
        &mut self,
        packet: &mut [u8],
    ) -> std::result::Result<Option<usize>, CuStreamRxError> {
        let count = self.rx.try_recv(packet)?;
        if let Some(n) = count {
            self.bytes.fetch_add(n as u64, Ordering::Relaxed);
        }
        Ok(count)
    }
}

#[derive(Default)]
struct Camera {
    image: Option<egui::ColorImage>,
    decoded: u64,
    encoded: u64,
    updated: Option<Instant>,
    errors: u64,
    bytes: u64,
    max_bytes: usize,
}
struct Shared {
    cameras: [Mutex<Camera>; 6],
    status: Mutex<CuTwinStatus>,
    bytes: Arc<AtomicU64>,
    stop: AtomicBool,
    failure: Mutex<Option<String>>,
}
impl Shared {
    fn new() -> Self {
        Self {
            cameras: std::array::from_fn(|_| Mutex::new(Camera::default())),
            status: Mutex::new(CuTwinStatus::default()),
            bytes: Arc::new(AtomicU64::new(0)),
            stop: AtomicBool::new(false),
            failure: Mutex::new(None),
        }
    }
}

struct Decoder {
    pipeline: gst::Pipeline,
    source: AppSrc,
    waiting_idr: bool,
}
impl Decoder {
    fn new(index: usize, shared: Arc<Shared>) -> Result<Self> {
        let pipeline = gst::parse::launch(
            "appsrc name=input is-live=true format=time do-timestamp=true block=false max-buffers=4 ! video/x-h265,stream-format=byte-stream,alignment=au ! h265parse ! avdec_h265 max-threads=2 ! videoconvert ! videoscale ! video/x-raw,format=RGB,width=960,height=540 ! appsink name=output sync=false max-buffers=1 drop=true"
        )?.downcast::<gst::Pipeline>().map_err(|_| "decoder pipeline")?;
        let source = pipeline
            .by_name("input")
            .ok_or("appsrc")?
            .downcast::<AppSrc>()
            .map_err(|_| "appsrc type")?;
        source.set_caps(Some(
            &gst::Caps::builder("video/x-h265")
                .field("stream-format", "byte-stream")
                .field("alignment", "au")
                .build(),
        ));
        let sink = pipeline
            .by_name("output")
            .ok_or("appsink")?
            .downcast::<AppSink>()
            .map_err(|_| "appsink type")?;
        sink.set_callbacks(
            AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let map = sample
                        .buffer()
                        .ok_or(gst::FlowError::Error)?
                        .map_readable()
                        .map_err(|_| gst::FlowError::Error)?;
                    // RGB 960 has a tightly packed, 4-byte-aligned stride (2880).
                    if map.len() != 960 * 540 * 3 {
                        return Err(gst::FlowError::Error);
                    }
                    let image = egui::ColorImage::from_rgb([960, 540], map.as_slice());
                    let mut camera = shared.cameras[index].lock().unwrap();
                    camera.image = Some(image);
                    camera.decoded += 1;
                    camera.updated = Some(Instant::now());
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        pipeline.set_state(gst::State::Playing)?;
        Ok(Self {
            pipeline,
            source,
            waiting_idr: true,
        })
    }
    fn push(&mut self, bytes: &[u8]) -> Result<()> {
        // Rejoin only on a clean random-access picture after transport or view loss.
        if self.waiting_idr {
            let irap = bytes
                .windows(4)
                .any(|w| w[0..3] == [0, 0, 1] && (16..=21).contains(&((w[3] >> 1) & 63)));
            if !irap {
                return Ok(());
            }
            self.waiting_idr = false;
        }
        // Never retain an arbitrarily long decoder backlog.
        if self.source.current_level_buffers() >= 4 {
            self.reset()?;
            return Ok(());
        }
        self.source
            .push_buffer(gst::Buffer::from_slice(bytes.to_vec()))?;
        Ok(())
    }
    fn reset(&mut self) -> Result<()> {
        self.pipeline.set_state(gst::State::Null)?;
        self.pipeline.set_state(gst::State::Playing)?;
        self.waiting_idr = true;
        Ok(())
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

struct Receiver {
    shared: Arc<Shared>,
    archive: PathBuf,
    worker: Option<JoinHandle<std::result::Result<CuTwinStatus, String>>>,
}
impl Receiver {
    fn start(args: &Args) -> Result<Self> {
        gst::init()?;
        let shared = Arc::new(Shared::new());
        let mut socket = CuUdpLogStreamConfig::new(args.listen);
        socket.remote_addr = Some(args.feedback_to);
        socket.recv_buffer_bytes = Some(16 * 1024 * 1024);
        let (feedback, rx) = socket.open()?;
        let rx = CountedRx {
            rx,
            bytes: shared.bytes.clone(),
        };
        let limits = SessionRouterLimits {
            max_record_bytes: 16 * 1024 * 1024,
            max_buffered_records: 64,
            ..Default::default()
        };
        let archive = args.archive.clone().unwrap_or_else(|| {
            PathBuf::from(format!(
                "logs/telemetry-{}.copper",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
        });
        eprintln!("Copper log: {}", archive.display());
        let (twin, mut reader) = Twin::twin(rx)
            .with_feedback(feedback.ok_or("feedback transport")?)
            .with_log_path(&archive)
            .with_receiver_limits(limits)
            .with_section_size(17 * 1024 * 1024)
            .with_slab_size(1024 * 1024 * 1024)
            .with_replay_capacity(128.try_into()?)
            .with_frame_capacity(32.try_into()?)
            .spawn()?;
        let mut decoders: Vec<_> = (0..6)
            .map(|i| Decoder::new(i, shared.clone()))
            .collect::<Result<_>>()?;
        let state = shared.clone();
        let worker = std::thread::Builder::new()
            .name("camera-decode-feed".into())
            .spawn(move || {
                let result = (|| {
                    let mut twin: CuTwin<Twin> = twin;
                    let mut previous = None;
                    while !state.stop.load(Ordering::Relaxed) {
                        while let Some(read) = reader.try_read() {
                            let frame = read.frame;
                            let id = frame.copperlist.id;
                            if read.missed > 0 || previous.is_some_and(|old| old + 1 != id) {
                                for decoder in &mut decoders {
                                    decoder.reset().map_err(|e| e.to_string())?;
                                }
                            }
                            previous = Some(id);
                            let data = &frame.copperlist.msgs.0;
                            let messages = [&data.0, &data.1, &data.2, &data.3, &data.4, &data.5];
                            for (i, msg) in messages.iter().enumerate() {
                                if let Some(payload) = msg.payload() {
                                    let bytes =
                                        payload.map_readable().map_err(|e| e.to_string())?;
                                    {
                                        let mut camera = state.cameras[i].lock().unwrap();
                                        camera.encoded += 1;
                                        camera.bytes += bytes.len() as u64;
                                        camera.max_bytes = camera.max_bytes.max(bytes.len());
                                    }
                                    decoders[i]
                                        .push(bytes.as_slice())
                                        .map_err(|e| e.to_string())?;
                                }
                            }
                        }
                        *state.status.lock().unwrap() = reader.status();
                        for (i, decoder) in decoders.iter().enumerate() {
                            if let Some(bus) = decoder.pipeline.bus() {
                                for message in bus.iter() {
                                    if let gst::MessageView::Error(error) = message.view() {
                                        state.cameras[i].lock().unwrap().errors += 1;
                                        eprintln!(
                                            "camera {i}: {} {:?}",
                                            error.error(),
                                            error.debug()
                                        );
                                    }
                                }
                            }
                        }
                        if reader.is_closed() {
                            break;
                        }
                        reader.wait_timeout(Duration::from_millis(2));
                    }
                    let status = twin.stop().map_err(|e| e.to_string());
                    drop(decoders);
                    status
                })();
                if let Err(error) = &result {
                    *state.failure.lock().unwrap() = Some(error.clone());
                }
                result
            })?;
        Ok(Self {
            shared,
            archive,
            worker: Some(worker),
        })
    }
    fn stop(&mut self) -> Result<CuTwinStatus> {
        self.shared.stop.store(true, Ordering::Relaxed);
        let result = self
            .worker
            .take()
            .ok_or("receiver stopped")?
            .join()
            .map_err(|_| "receiver thread panicked")?;
        result.map_err(Into::into)
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        if self.worker.is_some() {
            let _ = self.stop();
        }
    }
}

struct View {
    receiver: Receiver,
    textures: [Option<egui::TextureHandle>; 6],
    counts: [u64; 6],
    fps: [f64; 6],
    sampled: Instant,
    last_bytes: u64,
    mbps: f64,
    deadline: Option<Instant>,
}
impl eframe::App for View {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        if self.deadline.is_some_and(|at| Instant::now() >= at) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let shared = &self.receiver.shared;
        let elapsed = self.sampled.elapsed().as_secs_f64();
        if elapsed >= 1.0 {
            let bytes = shared.bytes.load(Ordering::Relaxed);
            self.mbps = (bytes - self.last_bytes) as f64 * 8.0 / elapsed / 1e6;
            self.last_bytes = bytes;
            for i in 0..6 {
                let count = shared.cameras[i].lock().unwrap().decoded;
                self.fps[i] = (count - self.counts[i]) as f64 / elapsed;
                self.counts[i] = count;
            }
            self.sampled = Instant::now();
        }
        let status = *shared.status.lock().unwrap();
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label(format!("Recording: {}", self.receiver.archive.display()));
            if let Some(error) = &*shared.failure.lock().unwrap() {
                ui.colored_label(egui::Color32::RED, error);
            }
            ui.label(format!(
                "{:.1} Mbit/s   {:?}   gaps {}   replay drops {}   feedback {}",
                self.mbps,
                status.twin.state,
                status.gaps,
                status.twin.queue_overflows,
                status.feedback_reports_sent
            ));
            let size = egui::vec2(
                (ui.available_width() - 24.0) / 3.0,
                (ui.available_height() - 64.0) / 2.0,
            );
            egui::Grid::new("cameras").num_columns(3).show(ui, |ui| {
                for i in 0..6 {
                    ui.vertical(|ui| {
                        let mut camera = shared.cameras[i].lock().unwrap();
                        if let Some(image) = camera.image.take() {
                            if let Some(texture) = &mut self.textures[i] {
                                texture.set(image, egui::TextureOptions::LINEAR);
                            } else {
                                self.textures[i] = Some(ctx.load_texture(
                                    format!("camera{i}"),
                                    image,
                                    egui::TextureOptions::LINEAR,
                                ));
                            }
                        }
                        let age = camera
                            .updated
                            .map_or(0.0, |at| at.elapsed().as_secs_f64() * 1000.0);
                        ui.label(format!(
                            "Camera {}  {:.1} fps  {:.0} ms since decode",
                            i + 1,
                            self.fps[i],
                            age
                        ));
                        if let Some(texture) = &self.textures[i] {
                            ui.add(
                                egui::Image::new(texture)
                                    .fit_to_exact_size(size)
                                    .maintain_aspect_ratio(true),
                            );
                        } else {
                            ui.allocate_space(size);
                        }
                    });
                    if i % 3 == 2 {
                        ui.end_row();
                    }
                }
            });
        });
        ctx.request_repaint_after(Duration::from_millis(8));
    }
}
fn main() -> Result<()> {
    let args = Args::parse();
    let mut receiver = Receiver::start(&args)?;
    if args.headless {
        let started = Instant::now();
        for _ in 0..(if args.seconds == 0 { 30 } else { args.seconds }) {
            std::thread::sleep(Duration::from_secs(1));
            if receiver
                .worker
                .as_ref()
                .is_some_and(|worker| worker.is_finished())
            {
                break;
            }
            let state = receiver.shared.status.lock().unwrap();
            let counts: Vec<_> = receiver
                .shared
                .cameras
                .iter()
                .map(|c| {
                    let c = c.lock().unwrap();
                    (c.encoded, c.decoded, c.errors, c.bytes, c.max_bytes)
                })
                .collect();
            eprintln!(
                "t={:.0}s cameras(encoded,decoded,errors,bytes,max_au)={counts:?} bytes={} gaps={} replay={} feedback={}",
                started.elapsed().as_secs_f64(),
                receiver.shared.bytes.load(Ordering::Relaxed),
                state.gaps,
                state.twin.reconstructed,
                state.feedback_reports_sent
            );
        }
        let status = receiver.stop()?;
        eprintln!("final {status:?}");
        if receiver
            .shared
            .cameras
            .iter()
            .any(|c| c.lock().unwrap().decoded == 0)
        {
            return Err("one or more cameras decoded no frames".into());
        }
    } else {
        let view = View {
            receiver,
            textures: std::array::from_fn(|_| None),
            counts: [0; 6],
            fps: [0.0; 6],
            sampled: Instant::now(),
            last_bytes: 0,
            mbps: 0.0,
            deadline: (args.seconds > 0)
                .then(|| Instant::now() + Duration::from_secs(args.seconds)),
        };
        eframe::run_native(
            "Copper · six cameras",
            eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default().with_inner_size([1600.0, 980.0]),
                ..Default::default()
            },
            Box::new(|_| Ok(Box::new(view))),
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}
