# Copper multistream telemetry demo

Six 1080p cameras on a Jetson stream NVENC-compressed H.265 through Copper's UDP
live-twin transport to a desktop GUI. The tested configuration delivers **30 fps
per camera at about 137 Mbit/s**, with upstream feedback enabled and no robot
recording.

One Cargo workspace contains both entry points:

| Command, from the workspace root | Runs on | Entry point |
| --- | --- | --- |
| `just` (or `cargo run -r`) | Jetson robot | `src/main.rs`: capture six cameras, encode with NVENC, send Copper telemetry |
| `just telemetry` (or `cargo run -r -p telemetry-station`) | Desktop workstation | `telemetry-station/src/main.rs`: receive the typed twin, decode, display six streams |

The root package is the default workspace member. Building the robot does not
build eframe or the desktop windowing dependencies. Both packages use the shared
camera graph in `src/lib.rs`, the same `copperconfig.ron`, and one `Cargo.lock`.
For Cargo commands, arguments after `--` go to the selected application.
The just recipes forward arguments directly: `just telemetry --headless --seconds 35`
or `just default --fps 15`. `just fmt` formats both packages.

## Layout and prerequisites

Keep a compatible Copper checkout next to this project:

```text
parent/
├── copper-rs/
└── cu-multistream-telemetry-demo/
    ├── Cargo.toml               # workspace and default robot package
    ├── copperconfig.ron        # camera graph and UDP/FEC/feedback configuration
    ├── src/lib.rs              # shared graph, camera paths, NVENC pipeline
    ├── src/main.rs             # robot executable
    └── telemetry-station/
        ├── Cargo.toml
        └── src/main.rs         # desktop executable
```

All Copper dependencies are local paths under `../copper-rs`, so the runtime can
be patched in place. Copy this entire project, including the station subdirectory
and lockfile, to share it. Build outputs under `target/` are unnecessary. Use
matching Copper source versions at both ends when changing the graph or protocol.

The prepared Delta kit has Rust, native C/C++ tools, GStreamer development
headers, `pkg-config`, and NVIDIA plugins through `/data/dev/env.sh`. For a
noninteractive SSH build, use `/data/dev/bin/dev-shell`; ordinary SSH commands
do not load that environment. The desktop needs Rust, GStreamer development
libraries, `h265parse`, `avdec_h265`, `videoconvert`, `videoscale`, and a graphical
session. The existing kit setup guide is `deltakit-dev-extra.md` beside the
workstation project; its accompanying assets reproduce the development tools.

Installed locations for this kit:

- Workstation: `/home/gbin/projects/copper/yaak/cu-multistream-telemetry-demo`.
- Delta: `/root/cu-multistream-telemetry-demo` (uses `/root/copper-rs`).
- On this workstation, `yaak/copper-rs` is a symlink to the existing
  `/home/gbin/projects/copper/copper-rs` checkout, preserving the sibling layout.

## Start the demo

**1. Start the station on the workstation:**

```bash
cd /home/gbin/projects/copper/yaak/cu-multistream-telemetry-demo
just telemetry
```

**2. Start the robot in another terminal:**

```bash
ssh delta
cd /root/cu-multistream-telemetry-demo
just
```

The default recipe loads `/data/dev/env.sh` when present, raises the runtime
UDP buffer ceilings to 64 MiB send / 16 MiB receive, and launches
`cargo +stable run -r --locked`. It does not write persistent sysctl configuration.
Arguments are forwarded to the robot executable. Bare `cargo run -r` remains
available when the development environment and buffer limits are already prepared.

To launch directly from the workstation through the kit's development shell:

```bash
ssh -t delta /data/dev/bin/dev-shell just --justfile /root/cu-multistream-telemetry-demo/justfile
```

The robot defaults to sending to `192.168.1.3:7447`; the station listens on
`0.0.0.0:7447` and returns feedback to `192.168.1.145:7448`. These are this kit's
wired-LAN addresses. For another network, replace them explicitly:

```bash
# Station
cargo run -r -p telemetry-station -- --feedback-to ROBOT_IP:7448
# Robot
cargo run -r -- --remote STATION_IP:7447
```

Use numeric IP addresses. Allow UDP 7447 toward the station and UDP 7448 toward
the robot. Start the station first. Close and reopen the station after restarting
the robot: one twin receiver currently accepts one sender session. Ctrl-C stops
the robot's cameras cleanly; closing the GUI stops its receiver and decoders.
Use `cargo run -r -- --help` or `cargo run -r -p telemetry-station -- --help`
for the complete options.

## Defaults and tuning

The six sources capture 1920×1080 UYVY at 30 fps, convert to NV12, and use NVENC
H.265 CBR at 20 Mbit/s per camera. No B frames are used; parameter sets repeat at
IDRs. This mode is lossy, including the 4:2:2 to 4:2:0 chroma conversion. The
existing `CuGstBuffer` payload carries complete Annex-B access units, so no new
Copper logging codec is needed.

Copper polls at 120 Hz, explicitly pacing `run_one_iteration` outside task
execution. Capture rings hold two frames. The link uses 1200-byte Copper packets,
a 200 Mbit/s shared packet budget, 120 ms expiry, a 16-symbol GF256 FEC window,
and initially one repair per 32 source symbols. Feedback reports every 100 ms
and adapts repair density; it does not automatically change camera frame rate.
The robot requests a 32 MiB socket send buffer. The BSP's original 208 KiB
buffer ceiling caused drops, which is why startup raises it.

If bandwidth becomes limited, lower frame rate or camera bitrate and restart
both ends. For example, on the robot:

```bash
cargo run -r -- --fps 15 --camera-bitrate 10000000 --link-mbps 120
```

`--link-mbps` includes FEC and recovery overhead and must leave room above the
six encoded streams. `--latency-ms` controls queue expiry. Watch the robot's
`tx_drops`, `expired`, `inbox_drops`, and feedback loss, and the station's fps,
gaps, and replay drops. Sender storage is bounded at 512 MiB to accommodate the
16 MiB record bound, record pools, and recovery-boundary packet sets; GStreamer
and NVENC retain additional raw surfaces.

The station decodes each full-resolution stream with two `avdec_h265` threads,
then scales it to a 960×540 texture for the three-by-two display. Receive and
replay workers run independently of the GUI. Each decoder queues at most four
access units and resumes at a random-access picture after a discontinuity.
The latest image replaces the previous one. Tile age means time since decode,
not measured sensor-to-display latency.

## Recording behavior

Robot task/keyframe logging is disabled and the builder uses `NoopLogger`.
Recovery keyframes still travel over UDP. Status is printed to the terminal;
no robot video, Copper recording, or status file is created.

Recording begins when the robot stream arrives. The station records the full
received native Copper log in `logs/`, relative to
its working directory. Starting from the workspace root produces, for example,
`logs/telemetry-1792253000000000000.copper`. Each run chooses a unique timestamp;
recordings survive station shutdown and are not overwritten or deleted. The GUI
and terminal display the recording path.

The base `.copper` path is accompanied by numbered backing slabs such as
`telemetry-<timestamp>_0.copper`, `_1.copper`, and so on. Keep the base path and
all of its slabs together when copying or reading a recording. The station adds
1 GiB slabs as needed; it records the entire session rather than rolling over.
At the measured stream rate, allow roughly 1 GB of storage per minute.

To choose another fresh log base path:

```bash
cargo run -r -p telemetry-station -- --archive logs/customer-session.copper
```

An existing archive path is rejected to protect earlier recordings. `logs/` is
excluded from source control. Native capture includes the received compressed
camera payloads and recovery data, independently of GUI consumption. It cannot
restore data lost before reception. The station uses 17 MiB archive sections,
a 128-event replay queue for recovery arrival, and a 32-CopperList view queue.

## Camera mapping

Use the BSP's stable paths rather than relying on video-device enumeration.

| GUI camera | Module / sensor | Device |
| --- | --- | --- |
| 1 | Single, first | `/dev/cams/des0/port0/sensor0` |
| 2 | Triple, first | `/dev/cams/des0/port1/sensor0` |
| 3 | Triple, second | `/dev/cams/des0/port1/sensor1` |
| 4 | Triple, third | `/dev/cams/des0/port1/sensor2` |
| 5 | Dual, first | `/dev/cams/des1/port0/sensor0` |
| 6 | Dual, second | `/dev/cams/des1/port0/sensor1` |

## Validation and diagnostics

The initial six-camera test on 2026-10-17 measured **30.0 decoded fps on every
camera** and **137.4 Mbit/s of received UDP payload** over an 11-second steady
window, with zero Copper gaps, replay queue overflows, or decode errors. Feedback
reached Active. The subsequent GUI test showed roughly 135–141 Mbit/s with zero
sender drops or expired packets. That test used the original 1000 Mbit/s link. The current USB adapter on Delta
(`eth1`, `192.168.1.145`) negotiates 2500 Mbit/s. No perceivable
lag was reported during interactive viewing; absolute end-to-end latency has
not been instrumented. Scene content and network conditions affect bandwidth.

After merging the workspace, a further recording test archived and replayed
2,488 CopperLists with zero gaps, replay queue overflows, or decode errors.
The full recording remained in `logs/` after shutdown and crossed into a second
256 MiB slab (the earlier slab size), confirming that session data is retained
without rollover. The current default uses 1 GiB slabs to reduce file turnover.

Run a finite test using the same receiver and decoder paths as the GUI:

```bash
# Workstation
cargo run -r -p telemetry-station -- --headless --seconds 35
# Robot, in another terminal
cargo run -r -- --seconds 30
```

The station lists per-camera `(encoded, decoded, errors, bytes, max_au)` counters,
plus receive/replay status, and fails if any camera decodes no frames.
`--seconds 30` also works with the GUI for a timed run. Use `just fmt` to format
both packages and `cargo clippy --workspace --all-targets -- -D warnings` to
check the workspace. No Copper core patch was needed for this demo.

The prepared Delta environment includes Wireshark 4.6.9 CLI tools. In its
development shell, inspect bandwidth without recording packets:

```bash
tshark -i eth0 -f 'udp port 7447 or udp port 7448' -q -z io,stat,1
```

## Experimental near-lossless mode

For CPU-based, byte-exact compression of native UYVY, see the standalone
[LZ4/Zstd benchmark](compression-bench/README.md). It measures compression ratio,
preprocessing cost, and one-worker versus six-worker throughput without changing
the live video transport.

For the 2.5 Gbit/s adapter, use YUV444 H.265 with a fixed low quantizer and the
lossless encoder switch disabled:

```bash
# Workstation: start a fresh station before each robot run.
cargo run -r -p telemetry-station
# Delta: 15 fps, QP 6, 2000 Mbit/s transport budget, 800 ms expiry.
cargo run -r --features near-lossless
```

The equivalent runtime option is `--near-lossless`. Set `--qp` between 1 and 51
(default 6);
lower values improve quality and increase bandwidth. `--camera-bitrate` applies
only to the original CBR mode. Fixed QP does not cap the encoder's output rate;
`--link-mbps` only paces the transport and cannot prevent overload if camera
output exceeds its budget. The maximum accepted link budget is now 2300 Mbit/s.
Explicit mode flags override feature defaults; if both encoding features are
built, near-lossless is the default. The default build remains 30 fps YUV420 CBR.

Near-lossless uses NV24/YUV444 to avoid the original NV12 chroma downsampling,
`profile=3`, `ratecontrol-enable=false`, `preset-level=0`, and the same QP for I/P/B
frames. It keeps the original buffer ownership path; immediate copying remains
exclusive to true lossless mode. These controls match the installed plugin and
[NVIDIA's encoder documentation](https://docs.nvidia.com/metropolis/deepstream/dev-guide/text/DS_plugin_gst-nvvideo4linux2.html).
It is lossy encoding; "near-lossless" names an experimental quality preset,
not a bound on pixel error. UYVY-to-NV24 conversion is not proven reversible.

The recommended QP 6 / 15 fps trial completed 20 seconds with all six cameras.
Over a ten-second steady window, each camera delivered approximately 15 encoded
and decoded fps, averaging 565 Mbit/s received UDP payload. Sender drops, expiry,
receiver gaps, replay overflows, and decoder error counters stayed zero.
Some frames were skipped during decoder startup. The station archived 2382
CopperLists. Re-encoding the same 56-frame YUV444 reference at QP 6 measured
55.74 dB average YUV PSNR (53.20 dB luma). These are short tests of the current
scene, not guarantees for arbitrary motion or long sessions.

A 56-frame camera sample at QP 10 measured 53.25 dB average YUV PSNR
(50.44 dB luma) against the pre-encoder YUV444 reference, matching frames by
index. This measures codec error after conversion, not fidelity to raw camera
UYVY. The six-camera QP 10 / 15 fps run completed 20 seconds without a crash,
sender drops, expired packets, receiver gaps, or replay queue overflows.
Steady sender traffic was approximately 350–410 Mbit/s. All cameras decoded;
some decoder resets/skips occurred during startup, followed by approximately
15 decoded fps per camera. The station archived 2327 CopperLists.

A QP 6 run requested 30 fps for 25 seconds and completed without a crash,
sender drops, receiver gaps, or replay queue overflows, at approximately
780–810 Mbit/s. However, delivered encoded rates settled near 22 fps per camera,
and several software decoders skipped frames under load. This is not a validated
30 fps mode; increasing network bandwidth alone does not resolve those limits.

## Experimental lossless mode

Build with the `lossless` feature to select NV24/YUV444 lossless encoding,
15 fps per camera, a 900 Mbit/s link budget, and 800 ms packet expiry:

```bash
cargo run -r --features lossless
```

Without either encoding feature, the defaults remain lossy CBR at 30 fps, a 200 Mbit/s
link budget, and 120 ms expiry. The existing `--experimental-lossless` (alias
`--lossless`) runtime option selects the same lossless defaults. Explicit
`--fps`, `--link-mbps`, and `--latency-ms` override either mode's defaults.

Only lossless mode sets the GStreamer source's `copy_buffers` option. Each
encoded access unit is copied into owned bytes in the appsink callback before
it enters the Copper ring, releasing the NVIDIA buffer promptly. Lossy mode
retains the original GStreamer buffers. This requires the companion changes in
`../copper-rs` (the GStreamer source and boolean config-value conversion) on both
machines; the wire payload format is unchanged.

**Immediate copying has not fixed the native encoder crash.** On 2026-10-17,
the patched six-camera application reproduced `gst_buffer_resize_range` and
heap-corruption failures at both 15 fps and 5 fps. A short six-process standalone
GStreamer test at 5 fps completed; that does not establish long-term stability.
The feature remains experimental.

A separate ten-frame 1080p YUV444 test encoded and decoded to identical SHA-256
hashes, confirming that the encoder can produce pixel-exact H.265. Preservation
of the original camera's UYVY samples through NV24 conversion is still unverified.
The GUI also scales and converts decoded images for display.

Measured camera access units were roughly 1.2–2.35 MB. At approximately 1.4 MB
per frame, six cameras at 15 fps need about 1 Gbit/s before transport overhead;
15 fps is therefore a trial default, not a rate proven to fit the wired link.
Lower frame rates may be necessary even after the native crash is resolved.

A [similar NVIDIA buffer report](https://forums.developer.nvidia.com/t/nvv4l2h265enc-lossless-bug/304970)
is a diagnostic lead, not an established cause for this BSP. NVIDIA's
[accelerated GStreamer guide](https://docs.nvidia.com/jetson/archives/r36.3/DeveloperGuide/SD/Multimedia/AcceleratedGstreamer.html)
describes NV24 hardware encoding. The robot's NVIDIA driver/plugin was not replaced.
