//! Shared camera graph for the robot and its typed Copper twin.
#[cfg(feature = "server")]
use cu29::prelude::*;
extern crate self as cu_multistream_telemetry_demo;

#[cfg(feature = "server")]
#[copper_runtime(config = "copperconfig.ron")]
struct Server {}
#[cfg(feature = "server")]
pub type Robot = default::Server;

#[cfg(feature = "twin")]
pub mod twin {
    use cu29::prelude::*;
    #[copper_runtime(config = "copperconfig.ron", sim_mode = true)]
    struct Replay {}
    pub type Twin = default::Replay;
    pub type DataSet = default::CuStampedDataSet;
}

/// Stable BSP paths, independent of video enumeration.
pub const CAMERAS: [&str; 6] = [
    "/dev/cams/des0/port0/sensor0",
    "/dev/cams/des0/port1/sensor0",
    "/dev/cams/des0/port1/sensor1",
    "/dev/cams/des0/port1/sensor2",
    "/dev/cams/des1/port0/sensor0",
    "/dev/cams/des1/port0/sensor1",
];

/// Hardware encoding settings selected before the cameras start.
#[derive(Clone, Copy, Debug)]
pub enum Encoding {
    /// Bitrate-controlled YUV420, matching the original demo.
    Cbr { bitrate: u32 },
    /// YUV444 with a fixed quantizer; lower QP improves quality and uses more bandwidth.
    NearLossless { qp: u32 },
    /// Experimental pixel-exact YUV444 encoding.
    Lossless,
}

/// NVENC emits complete Annex-B access units; parameter sets repeat at every IDR.
pub fn pipeline(device: &str, fps: u32, encoding: Encoding) -> String {
    let (format, encoder) = match encoding {
        Encoding::Lossless => ("NV24", "enable-lossless=true profile=3".to_owned()),
        Encoding::NearLossless { qp } => (
            "NV24",
            format!(
                "enable-lossless=false profile=3 ratecontrol-enable=false preset-level=0 quant-i-frames={qp} quant-p-frames={qp} quant-b-frames={qp}"
            ),
        ),
        Encoding::Cbr { bitrate } => (
            "NV12",
            format!("bitrate={bitrate} control-rate=1 preset-level=1"),
        ),
    };
    format!(
        "v4l2src device={device} do-timestamp=true ! video/x-raw,format=UYVY,width=1920,height=1080,framerate=30/1 ! videorate drop-only=true ! video/x-raw,framerate={fps}/1 ! nvvidconv ! video/x-raw(memory:NVMM),format={format} ! nvv4l2h265enc {encoder} iframeinterval={fps} idrinterval={fps} insert-sps-pps=true insert-aud=true num-B-Frames=0 ! h265parse ! video/x-h265,stream-format=byte-stream,alignment=au ! appsink name=copper sync=false max-buffers=2 drop=true"
    )
}

/// Holds Copper's existing worker counters; sampling happens outside task execution.
pub struct StreamMonitor {
    pub streams: Option<std::sync::Arc<[cu29::monitoring::LogStreamMonitor]>>,
}
impl cu29::monitoring::CuMonitor for StreamMonitor {
    fn new(
        _: cu29::monitoring::CuMonitoringMetadata,
        runtime: cu29::monitoring::CuMonitoringRuntime,
    ) -> cu29::prelude::CuResult<Self> {
        Ok(Self {
            streams: runtime.log_streams(),
        })
    }
    fn process_copperlist(
        &self,
        _: &cu29::prelude::CuContext,
        _: cu29::monitoring::CopperListView<'_>,
    ) -> cu29::prelude::CuResult<()> {
        Ok(())
    }
    fn process_error(
        &self,
        _: cu29::monitoring::ComponentId,
        _: cu29::monitoring::CuComponentState,
        _: &cu29::prelude::CuError,
    ) -> cu29::monitoring::Decision {
        cu29::monitoring::Decision::Shutdown
    }
}
