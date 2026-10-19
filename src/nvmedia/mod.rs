//! Rust NvMedia IEP backend with a 4K bitstream reservation for lossless 1080p.
mod encoder;
mod ffi;
mod gpu;
use cu_gstreamer::CuGstBuffer;
use cu29::prelude::*;
pub use encoder::EncodeTimings;
pub use encoder::Encoder;
use gstreamer::prelude::*;
use gstreamer_video::VideoFrameExt;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

/// Capture native UYVY without passing through NVIDIA's V4L2 encoder/converter.
pub fn capture_pipeline(device: &str, fps: u32) -> String {
    format!(
        "v4l2src device={device} io-mode=dmabuf do-timestamp=true ! video/x-raw,format=UYVY,width=1920,height=1080,framerate=30/1 ! videorate drop-only=true ! video/x-raw,framerate={fps}/1 ! appsink name=copper sync=false max-buffers=2 drop=true wait-on-eos=false"
    )
}

/// Expand UYVY 4:2:2 into planar 4:4:4 by duplicating chroma, preserving every sample.
/// `stride` includes any row padding; `output` must already hold width×height×3 bytes.
pub fn uyvy_to_y444(
    input: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    output: &mut [u8],
) -> CuResult<()> {
    let plane = width
        .checked_mul(height)
        .ok_or(CuError::from("Image size overflow"))?;
    let row = width
        .checked_mul(2)
        .ok_or(CuError::from("Row size overflow"))?;
    let required = height
        .checked_sub(1)
        .and_then(|n| n.checked_mul(stride))
        .and_then(|n| n.checked_add(row))
        .ok_or(CuError::from("Invalid UYVY dimensions"))?;
    if width == 0
        || !width.is_multiple_of(2)
        || stride < row
        || input.len() < required
        || Some(output.len()) != plane.checked_mul(3)
    {
        return Err("Invalid UYVY dimensions, stride, or buffer size".into());
    }
    let (y, uv) = output.split_at_mut(plane);
    let (u, v) = uv.split_at_mut(plane);
    for line in 0..height {
        let source = &input[line * stride..line * stride + row];
        for (pair, pixels) in source.as_chunks::<4>().0.iter().enumerate() {
            let i = line * width + pair * 2;
            y[i] = pixels[1];
            y[i + 1] = pixels[3];
            u[i] = pixels[0];
            u[i + 1] = pixels[0];
            v[i] = pixels[2];
            v[i + 1] = pixels[2];
        }
    }
    Ok(())
}

/// Read one mapped GStreamer UYVY frame, honoring its negotiated stride and metadata.
pub fn sample_to_y444(sample: &gstreamer::Sample, output: &mut [u8]) -> CuResult<()> {
    let caps = sample
        .caps()
        .ok_or(CuError::from("Camera sample has no caps"))?;
    let info = gstreamer_video::VideoInfo::from_caps(caps)
        .map_err(|e| CuError::new_with_cause("Invalid camera caps", e))?;
    if info.format() != gstreamer_video::VideoFormat::Uyvy
        || info.width() != 1920
        || info.height() != 1080
    {
        return Err("NvMedia capture requires 1920×1080 UYVY".into());
    }
    let buffer = sample
        .buffer()
        .ok_or(CuError::from("Camera sample has no buffer"))?;
    let frame = gstreamer_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
        .map_err(|e| CuError::new_with_cause("Cannot map camera frame", e))?;
    let stride = usize::try_from(frame.plane_stride()[0])
        .map_err(|_| CuError::from("Negative UYVY stride"))?;
    let bytes = frame
        .plane_data(0)
        .map_err(|e| CuError::new_with_cause("Cannot read UYVY plane", e))?;
    uyvy_to_y444(bytes, stride, 1920, 1080, output)
}

#[derive(Default)]
struct WorkerState {
    queue: VecDeque<CuGstBuffer>,
    error: Option<String>,
}

/// Copper camera source: DMA capture, three shared GPU input slots, two encoded frames.
#[derive(Reflect)]
#[reflect(from_reflect = false)]
pub struct CuNvMediaSource {
    pipeline: String,
    fps: u32,
    #[reflect(ignore)]
    state: Arc<Mutex<WorkerState>>,
    #[reflect(ignore)]
    stop: Arc<AtomicBool>,
    #[reflect(ignore)]
    worker: Option<JoinHandle<()>>,
}
impl Freezable for CuNvMediaSource {}
impl CuSrcTask for CuNvMediaSource {
    type Resources<'r> = ();
    type Output<'m> = output_msg!(CuGstBuffer);
    fn new(config: Option<&ComponentConfig>, _: Self::Resources<'_>) -> CuResult<Self> {
        let config = config.ok_or(CuError::from("NvMedia camera config required"))?;
        let pipeline = config
            .get::<String>("pipeline")?
            .ok_or(CuError::from("Camera pipeline required"))?;
        let fps = config.get::<u32>("fps")?.unwrap_or(15);
        if !(1..=30).contains(&fps) {
            return Err("Camera fps must be 1..=30".into());
        }
        Ok(Self {
            pipeline,
            fps,
            state: Arc::default(),
            stop: Arc::new(AtomicBool::new(false)),
            worker: None,
        })
    }
    fn start(&mut self, _: &CuContext) -> CuResult<()> {
        if self.worker.is_some() {
            return Err("NvMedia camera is already running".into());
        }
        *self
            .state
            .lock()
            .map_err(|_| CuError::from("Camera worker lock poisoned"))? = WorkerState::default();
        self.stop.store(false, Ordering::Relaxed);
        let state = self.state.clone();
        let stop = self.stop.clone();
        let pipeline = self.pipeline.clone();
        let fps = self.fps;
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        self.worker = Some(
            std::thread::Builder::new()
                .name("nvmedia-camera".into())
                .spawn(move || {
                    if let Err(error) = capture(&pipeline, fps, &stop, &state, &ready_tx) {
                        let message = error.to_string();
                        let _ = ready_tx.try_send(Err(message.clone()));
                        if let Ok(mut state) = state.lock() {
                            state.error = Some(message);
                        }
                    }
                })
                .map_err(|e| CuError::new_with_cause("Cannot start camera worker", e))?,
        );
        match ready_rx.recv_timeout(std::time::Duration::from_secs(40)) {
            Ok(Ok(())) => Ok(()),
            result => {
                self.stop_worker()?;
                Err(CuError::from(format!(
                    "NvMedia camera startup failed: {result:?}"
                )))
            }
        }
    }
    fn process(&mut self, ctx: &CuContext, output: &mut Self::Output<'_>) -> CuResult<()> {
        output.clear_payload();
        let mut state = self
            .state
            .lock()
            .map_err(|_| CuError::from("Camera worker lock poisoned"))?;
        if let Some(error) = &state.error {
            return Err(CuError::from(error.as_str()));
        }
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            return Err("Camera worker exited unexpectedly".into());
        }
        if let Some(frame) = state.queue.pop_front() {
            // This is delivery time, matching the existing demo source, not sensor time.
            output.tov = ctx.now().into();
            output.set_payload(frame);
        }
        Ok(())
    }
    fn stop(&mut self, _: &CuContext) -> CuResult<()> {
        self.stop_worker()
    }
}
impl CuNvMediaSource {
    fn stop_worker(&mut self) -> CuResult<()> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| CuError::from("NvMedia camera worker panicked"))?;
        }
        self.state
            .lock()
            .map_err(|_| CuError::from("Camera worker lock poisoned"))?
            .queue
            .clear();
        Ok(())
    }
}
impl Drop for CuNvMediaSource {
    fn drop(&mut self) {
        let _ = self.stop_worker();
    }
}

struct CapturePipeline(gstreamer::Pipeline);
impl Drop for CapturePipeline {
    fn drop(&mut self) {
        let _ = self.0.set_state(gstreamer::State::Null);
    }
}
fn capture(
    pipeline: &str,
    fps: u32,
    stop: &AtomicBool,
    state: &Mutex<WorkerState>,
    ready: &mpsc::SyncSender<Result<(), String>>,
) -> CuResult<()> {
    gstreamer::init().map_err(|e| CuError::new_with_cause("Cannot initialize GStreamer", e))?;
    let pipeline = CapturePipeline(
        gstreamer::parse::launch(pipeline)
            .map_err(|e| CuError::new_with_cause("Cannot parse capture pipeline", e))?
            .downcast::<gstreamer::Pipeline>()
            .map_err(|_| CuError::from("Expected capture pipeline"))?,
    );
    let sink = pipeline
        .0
        .by_name("copper")
        .ok_or(CuError::from("Capture appsink named copper required"))?
        .downcast::<gstreamer_app::AppSink>()
        .map_err(|_| CuError::from("Expected appsink named copper"))?;
    let bus = pipeline
        .0
        .bus()
        .ok_or(CuError::from("Camera bus unavailable"))?;
    let mut encoder = Encoder::new(fps)?;
    pipeline
        .0
        .set_state(gstreamer::State::Playing)
        .map_err(|e| CuError::new_with_cause("Cannot start camera capture", e))?;
    let mut ready_sent = false;
    let mut last_frame = std::time::Instant::now();
    while !stop.load(Ordering::Relaxed) {
        encoder.reclaim_inputs()?;
        // Preserve the encoded reference chain, including startup's first IDR.
        // If Copper falls behind, let appsink discard raw frames before encoding.
        if state
            .lock()
            .map_err(|_| CuError::from("Camera worker lock poisoned"))?
            .queue
            .len()
            == 2
        {
            last_frame = std::time::Instant::now();
            std::thread::park_timeout(std::time::Duration::from_millis(2));
            continue;
        }
        while let Some(message) = bus.pop() {
            if let gstreamer::MessageView::Error(error) = message.view() {
                return Err(CuError::from(format!(
                    "Camera capture: {} ({:?})",
                    error.error(),
                    error.debug()
                )));
            }
        }
        let Some(sample) = sink.try_pull_sample(gstreamer::ClockTime::from_mseconds(100)) else {
            if last_frame.elapsed() > std::time::Duration::from_secs(10) {
                return Err("Camera produced no frame for 10 seconds".into());
            }
            if sink.is_eos() {
                return Err("Camera capture reached EOS".into());
            }
            continue;
        };
        encoder.submit_sample(&sample)?;
        if encoder.queued() < encoder.capacity() {
            continue;
        }
        let mut encoded = Arc::<[u8]>::default();
        encoder.receive(&mut encoded)?;
        last_frame = std::time::Instant::now();
        let mut state = state
            .lock()
            .map_err(|_| CuError::from("Camera worker lock poisoned"))?;
        state.queue.push_back(CuGstBuffer::Replay(encoded));
        if !ready_sent {
            ready
                .send(Ok(()))
                .map_err(|_| CuError::from("Camera startup cancelled"))?;
            ready_sent = true;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_uyvy_preserves_extremes_chroma_and_padded_rows() {
        let input = [
            0, 1, 255, 2, 23, 24, 25, 26, 99, 99, 99, 99, 27, 28, 29, 30, 31, 32, 33, 34,
        ];
        let mut planar = [0; 24];
        uyvy_to_y444(&input, 12, 4, 2, &mut planar).unwrap();
        assert_eq!(&planar[..8], &[1, 2, 24, 26, 28, 30, 32, 34]);
        assert_eq!(&planar[8..16], &[0, 0, 23, 23, 27, 27, 31, 31]);
        assert_eq!(&planar[16..], &[255, 255, 25, 25, 29, 29, 33, 33]);
        let mut recovered = Vec::new();
        for i in (0..8).step_by(2) {
            recovered.extend_from_slice(&[planar[8 + i], planar[i], planar[16 + i], planar[i + 1]]);
        }
        assert_eq!(recovered, [&input[..8], &input[12..]].concat());
        assert!(uyvy_to_y444(&input[..19], 12, 4, 2, &mut planar).is_err());
        assert!(uyvy_to_y444(&input, 12, 3, 2, &mut planar).is_err());
    }
}
