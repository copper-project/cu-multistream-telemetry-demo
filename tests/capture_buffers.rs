//! Exercise both capture ownership paths without NVIDIA hardware.
use cu_gstreamer::{CuGStreamer, CuGstBuffer};
use cu29::prelude::*;
use std::time::{Duration, Instant};

#[test]
fn captured_bytes_survive_source_shutdown_in_both_modes() {
    for copy in [false, true] {
        let mut config = ComponentConfig::new();
        config.set(
            "pipeline",
            "fakesrc num-buffers=1 sizetype=fixed sizemax=16 filltype=zero ! appsink name=copper sync=false wait-on-eos=false".to_owned(),
        );
        config.set("caps", "application/octet-stream".to_owned());
        if copy {
            config.set("copy_buffers", true);
        }
        let mut source = CuGStreamer::<2>::new(Some(&config), ()).unwrap();
        let ctx = CuContext::new_with_clock();
        source.start(&ctx).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let payload = loop {
            let mut msg = CuMsg::<CuGstBuffer>::default();
            source.process(&ctx, &mut msg).unwrap();
            if let Some(payload) = msg.payload() {
                break payload.clone();
            }
            assert!(Instant::now() < deadline, "capture timed out (copy={copy})");
            std::thread::sleep(Duration::from_millis(1));
        };
        source.stop(&ctx).unwrap();
        drop(source);
        assert_eq!(payload.as_live().is_none(), copy);
        assert_eq!(payload.map_readable().unwrap().as_slice(), &[0; 16]);
    }
}
