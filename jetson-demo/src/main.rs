//! Six cameras, each with shared GPU preparation and independent lossless/CBR tasks.
//! Usage: cu-jetson-multicam-demo [seconds=10] [log-path=jetson-multicam.copper].
use cu_jetson_camera::CuEncodedFrame;
use cu29::prelude::*;
use std::time::{Duration, Instant};
#[derive(Default, Reflect)]
pub struct CountFrames {
    name: String,
    frames: usize,
    bytes: usize,
}
impl Freezable for CountFrames {}
impl CuSinkTask for CountFrames {
    type Resources<'r> = ();
    type Input<'m> = input_msg!(CuEncodedFrame);
    fn new(config: Option<&ComponentConfig>, _: ()) -> CuResult<Self> {
        let name = config
            .map(|c| c.get::<String>("name"))
            .transpose()?
            .flatten()
            .unwrap_or_default();
        Ok(Self {
            name,
            ..Self::default()
        })
    }
    fn process(&mut self, _: &CuContext, input: &Self::Input<'_>) -> CuResult<()> {
        if let Some(frame) = input.payload()
            && let Some(bytes) = &frame.bytes
        {
            self.frames += 1;
            self.bytes += bytes.with_inner(|b| b.as_ref().len());
        }
        Ok(())
    }
    fn stop(&mut self, _: &CuContext) -> CuResult<()> {
        eprintln!(
            "{}: {} frames, {} bytes",
            self.name, self.frames, self.bytes
        );
        Ok(())
    }
}
#[copper_runtime(config = "copperconfig.ron")]
struct App {}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let seconds: u64 = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(10);
    let path = args
        .get(2)
        .map(String::as_str)
        .unwrap_or("jetson-multicam.copper");
    let mut config = CuConfig::deserialize_ron(&default::App::original_config())?;
    if args.get(3).is_some_and(|arg| arg == "--no-log") {
        config
            .logging
            .as_mut()
            .ok_or("Missing logging config")?
            .enable_task_logging = false;
    }
    let app = default::App::builder()
        .with_config(config)
        .with_log_path(std::path::Path::new(path), Some(512 * 1024 * 1024))?
        .build()?;
    let mut running = app.start()?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(seconds) {
        let tick = Instant::now();
        running.run_one_iteration()?;
        std::thread::sleep(Duration::from_micros(4167).saturating_sub(tick.elapsed()));
    }
    let elapsed = started.elapsed().as_secs_f64();
    running.stop()?;
    eprintln!("Capture interval: {elapsed:.3}s");
    Ok(())
}
