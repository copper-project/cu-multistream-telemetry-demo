# NvMedia lossless alternative

Build with `--features nvmedia` to select the Rust NvMedia IEP backend. The
original CBR backend remains the default build. No DRIVE SDK headers or C shim
are needed: Rust loads the installed IEP/NvSci libraries using the ABI validated
by the [standalone probe](nvmedia-probe/README.md).

The initial default is **2 fps per camera**. Six-camera recording at requested
15 fps delivered only about 2 fps per camera in the first tests. The allocation
workaround is working; this implementation still needs throughput optimization.
`--fps` accepts 1–30 without promising that the encoder will sustain that rate.

## Local Copper recording on Delta

From `/root/cu-multistream-telemetry-demo` in the prepared development environment:

```bash
just record --output logs/nvmedia-trial.copper --seconds 30
```

This records all six cameras into a native Copper log, without starting UDP.
Choose a fresh log basename. Keep the base `.copper` path and all numbered slabs
together. Slabs are 1 GiB, sections are 64 MiB, and task-state keyframes are
disabled. Ctrl-C stops capture and closes the log. `--seconds 0` records until
interrupted; `--fps 2` explicitly selects the default rate.

Extract the recorded H.265 streams into a fresh directory:

```bash
cargo run -r --locked --features nvmedia --bin nvmedia-record -- extract \
  --input logs/nvmedia-trial.copper --output logs/nvmedia-trial-streams
```

The command reports CopperList, frame, and byte counts for each camera and fails
if any camera has no frames. Each `cam0.h265` through `cam5.h265` is a complete
Annex-B stream. For example, validate decoding of camera 0:

```bash
gst-launch-1.0 -q -e filesrc location=logs/nvmedia-trial-streams/cam0.h265 \
  ! h265parse ! avdec_h265 ! fakesink sync=false
```

## Byte-exact validation

The Rust checker records 30 deterministic full-range random YUV444 frames,
reads back every uploaded NvSciBuf surface, and checks that upload is exact:

```bash
cargo run -r --locked --features nvmedia --bin nvmedia-check -- \
  --output logs/nvmedia-byte-check --frames 30
```

Software-decode directly to Y444, without a color conversion, and compare bytes:

```bash
gst-launch-1.0 -q -e filesrc location=logs/nvmedia-byte-check/stream.h265 \
  ! h265parse ! avdec_h265 ! video/x-raw,format=Y444 \
  ! filesink location=logs/nvmedia-byte-check/decoded.y444 sync=false
cmp logs/nvmedia-byte-check/reference.y444 logs/nvmedia-byte-check/decoded.y444
```

Add `--camera /dev/cams/des0/port0/sensor0` and choose another fresh output
directory to validate real camera frames. Camera checks also save packed native
`reference.uyvy`; reconstruction from decoded Y444 takes every luma sample and
one sample from each identical adjacent U/V pair. `frames.csv` reports encoded
sizes and upload, hardware-wait, and retrieval times. Raw check artifacts are
large: the synthetic 30-frame test writes roughly 810 MB including decode output.

## Streaming

Start a fresh station on the workstation, allowing the larger record bound:

```bash
just telemetry --max-record-mib 64 --feedback-to 192.168.1.236:7448
```

Then start the alternative on Delta:

```bash
just nvmedia
```

The station example uses Delta's current `eth0` address, `192.168.1.236`;
replace `--feedback-to` if its address changes. This test used the 1 Gbit/s
`eth0` link. The existing address, duration, frame-rate, link, and latency
arguments still apply. Defaults are 2 fps, a 900 Mbit/s link budget, and
800 ms expiry. The NvMedia
sender reserves a 2 GiB transport memory budget and accepts 64 MiB Copper records
because one record can contain six large access units. The station's usual
16 MiB limit and original backend defaults remain available. Higher frame rates
and arbitrary scenes can overload capture, encoding, storage, or networking;
lossless coding does not guarantee delivery of every captured frame.

## Encoding and ownership

Each camera captures native 1920×1080 UYVY. Rust copies luma unchanged and
replicates each 4:2:2 chroma sample into two 4:4:4 samples. There is no color
matrix, scaling, range conversion, interpolation, or chroma reduction. This
expansion is reversible to the original UYVY samples.

Each camera worker owns an IEP instance and block-linear NvSciBuf input surface.
It creates at 3840×2160 with memory optimization disabled, sets DRC to 1920×1080
**before** `NvMediaIEPSetConfiguration`, and retains the initial allocation.
The measured internal bitstream capacity is 25,067,520 bytes rather than the
native 1080p allocation's 6,266,880 bytes. See the
[disassembly and runtime trace](nvmedia-probe/ALLOCATION.md).

Encoding uses 8-bit HEVC range extensions, lossless QP 0, the UHP preset,
no B frames, and an IDR with parameter sets for every frame. A separate 32 MiB
caller buffer receives `GetBits` output after capacity checks. CPU cache
coherency requirements are queried and honored. CPU-signaled pre-fences and
CPU-waited EOF fences protect each input surface. If asynchronous completion
fails, potentially live resources and their libraries remain allocated until
process exit; the encoder rejects further work.

GStreamer handles raw capture, frame selection, and appsink mapping; the NVIDIA
V4L2 encoder and `nvvidconv` are bypassed. Blocking upload and encoding happen
on camera workers. Raw appsink queues and encoded queues hold at most two
frames; overload drops older frames. Copper polls at 120 Hz without waiting for encoding,
clears absent outputs, and carries owned bytes using the existing `CuGstBuffer`
payload. As in the original demo, `tov` is delivery time rather than sensor time.

This backend checks for IEP 1.1.0, NvSciBuf 2.11, and NvSciSync 2.8 compatibility.
Testing is specific to Delta's Tegra234 / L4T R39.2.1 runtime. Installed NVIDIA
libraries are unchanged. ABI declarations follow the probe's linked public
[NVIDIA API reference](https://developer.nvidia.com/docs/drive/drive-os/7.0.3/public/drive-os-linux-sdk-api-ref/group__x__image__encode__api.html).

## Validation on Delta — 2026-10-19

[Evidence](nvmedia-probe/evidence/rust-demo-2026-10-19.json) includes source and
library hashes, byte comparisons, per-frame timings, decode results, test logs,
and streaming counters.

| Check | Result |
| --- | --- |
| 30 full-noise synthetic frames | Input, NvSciBuf readback, and decoded bytes identical; largest access unit 8,271,205 bytes |
| 30 frames from one real camera | Y444 decode exact; reconstructed native UYVY matches all 124,416,000 captured bytes |
| Six-camera Copper recording, 20 s at requested 2 fps | Extracted and decoded frame counts `[42, 42, 42, 42, 43, 41]`; zero decode errors |
| Six-camera streaming, 20 s at requested 2 fps | Decoded `[35, 35, 38, 38, 37, 35]`; zero decode errors; feedback Active |

The local recording counts include frames queued during camera startup. Streaming
still had 13 Copper gaps and 6 sender inbox drops, with zero transport drops,
expired packets, or twin queue overflows. Delivery of every frame remains
unvalidated.
An earlier streaming trial also encountered a camera capture timeout; startup
now waits for each camera's first successful encode and propagates worker failures.

Delta's release tests pass, including the existing GStreamer ownership test.
Workstation workspace clippy, the new conversion test, and doc tests pass.
The workstation's existing `capture_buffers` test fails its owned-copy assertion
because its sibling Copper checkout lacks the pre-existing `copy_buffers` patch;
the same test passes on Delta.
