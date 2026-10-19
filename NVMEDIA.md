# NvMedia lossless alternative

`--features nvmedia` selects the Rust NvMedia IEP backend. Camera DMA buffers
are imported directly into CUDA. A small CUDA kernel expands UYVY into a shared
block-linear NvSciBuf image, and IEP encodes that same allocation. Capture does
not map pixels on the CPU or call `NvSciBufObjPutPixels`.

The default is **15 fps per camera**. On Delta, one camera sustains 30 fps;
six concurrent cameras reached about 16 fps each in a capture/encode benchmark
without raw reference copies, disk recording, or networking. Those are measured
rates for the tested scenes, not a guarantee for arbitrary lossless content.

## Local Copper recording

On Delta, from `/root/cu-multistream-telemetry-demo`:

```bash
just record --output logs/nvmedia-trial.copper --seconds 30
```

This records six cameras into a native Copper log. Choose a fresh basename and
keep the base `.copper` path and numbered slabs together. Slabs are 1 GiB and
sections are 64 MiB. Ctrl-C closes the recording; `--seconds 0` runs until stopped.
`--fps` accepts 1–30.

Extract into a fresh directory:

```bash
cargo run -r --locked --features nvmedia --bin nvmedia-record -- extract \
  --input logs/nvmedia-trial.copper --output logs/nvmedia-trial-streams
```

Each `cam0.h265` through `cam5.h265` is an Annex-B stream. Validate software decode:

```bash
gst-launch-1.0 -q -e filesrc location=logs/nvmedia-trial-streams/cam0.h265 \
  ! h265parse ! avdec_h265 ! fakesink sync=false
```

## Byte-exact validation and benchmarking

The checker records deterministic full-range random YUV444, reads back the GPU
surface, and compares input bytes. It exercises the same CUDA surface stores and
NvSciSync fences as capture:

```bash
cargo run -r --locked --features nvmedia --bin nvmedia-check -- \
  --output logs/nvmedia-byte-check --frames 30
```

Decode to Y444 without color conversion, then compare:

```bash
gst-launch-1.0 -q -e filesrc location=logs/nvmedia-byte-check/stream.h265 \
  ! h265parse ! avdec_h265 ! video/x-raw,format=Y444 \
  ! filesink location=logs/nvmedia-byte-check/decoded.y444 sync=false
cmp logs/nvmedia-byte-check/reference.y444 logs/nvmedia-byte-check/decoded.y444
```

Add `--camera /dev/cams/des0/port0/sensor0` to validate real camera frames with
three submissions in flight. The checker saves native `reference.uyvy`, a CPU
reference conversion, GPU readback, and encoded output. These reference maps,
readbacks, and file writes are deliberately confined to validation. Its elapsed
time is not a capture throughput measurement.

For throughput, use `--benchmark` with a camera. It skips CPU pixel maps,
reference generation, readback, and encoded-file writes, keeping only timing CSV:

```bash
cargo run -r --locked --features nvmedia --bin nvmedia-check -- \
  --output logs/nvmedia-benchmark --camera /dev/cams/des0/port0/sensor0 \
  --frames 300 --fps 30 --benchmark
```

The CSV's `upload_us` measures host time for DMA import/cache lookup, kernel
submission, and GPU pre-fence signaling. `encode_us` includes queue residence;
it is not isolated hardware execution time. Validation artifacts are large.

## Streaming

On the workstation:

```bash
just telemetry --max-record-mib 64 --feedback-to 192.168.1.236:7448
```

On Delta:

```bash
just nvmedia
```

The current `eth0` path did **not** sustain the optimized stream in validation:
plain UDP at 400 Mbit/s delivered only 128,599 of 192,000 packets (33.0% loss),
without Copper, FEC, encoding, or decoding. The former 2.5 GbE USB interface is
absent. Local capture/encoding results below do not establish live delivery on
this network path. Streaming trials saw packet expiry and poor replay recovery;
the experimental transport/FEC changes used to isolate this were discarded.

The address is Delta's tested `eth0` address; update it if needed. Defaults are
15 fps, a 900 Mbit/s link budget, and 800 ms packet expiry. The sender permits
64 MiB Copper records and reserves a 2 GiB transport budget. Six lossless streams
can exceed a 1 Gbit/s link depending on the scene. Encoding rate, recorded rate,
and delivered/decoded rate are separate measurements.

## Memory, synchronization, and coding

Each camera worker owns three reusable NvSciBuf images and an ordered CUDA
stream. DMA imports are cached by allocation device/inode/size. CUDA owns a
duplicated fd; the cache does not retain extra GstMemory references that prevent
the V4L2 buffer pool from recycling its allocations. A submitted GstSample stays
alive until a CUDA event confirms conversion is complete. Reclaiming camera
buffers independently of encoder EOF prevents capture-pool starvation when
completed output waits for Copper.

The kernel copies every luma sample unchanged and duplicates each 4:2:2 chroma
sample into two 4:4:4 samples. There is no scaling, matrix/range conversion,
interpolation, or chroma reduction. Reconstructing UYVY from decoded Y444 restores
the original bytes.

CUDA signals an imported NvSciSync pre-fence after conversion, with NvSciBuf
memory synchronization enabled. IEP waits on that fence. CPU-waited encoder EOF
fences protect slot reuse. `GetBits` writes directly into its final `Arc<[u8]>`;
there is no 32 MiB staging buffer or subsequent Vec-to-Arc copy. Copper logging
and transport serialization still consume/copy compressed bytes as required.

IEP is created at 3840×2160 with memory optimization disabled, then DRC selects
1920×1080 **before** `NvMediaIEPSetConfiguration`. This retains the measured
25,067,520-byte internal bitstream capacity instead of the defective native
1080p allocation of 6,266,880 bytes. See the
[allocation investigation](nvmedia-probe/ALLOCATION.md).

Coding uses 8-bit HEVC range extensions, lossless QP 0, UHP, and no B frames.
The encoder selects P frames between IDRs, with an IDR and repeated parameter
sets every `fps` encoded frames. This reduces work and bandwidth compared with
the prototype's all-IDR stream. Losing an encoded reference frame can prevent
correct decoding until the next IDR; lossless coding does not guarantee delivery.

Raw appsink and completed-output queues each hold at most two frames. A full
completed queue pauses encoding so overload drops raw capture frames, preserving
the encoded reference chain and startup IDR. Copper polls at 120 Hz, carries
owned `CuGstBuffer` bytes, and uses delivery time for `tov`, matching the original demo. Worker errors are
propagated. Shutdown waits for submitted work; an uncertain asynchronous failure
retains potentially live resources and libraries until process exit.

Rust dynamically loads the installed IEP, NvSci, CUDA driver, NVRTC 13, and
GStreamer DMA APIs. No DRIVE headers, C shim, new dependency, or library patch is
needed to build. NVRTC compiles the embedded kernel for Orin's compute 8.7.
Runtime checks require IEP 1.1.0 and compatible NvSciBuf 2.11 / NvSciSync 2.8.
Tests are specific to Delta's Tegra234 / L4T R39.2.1 runtime.

## Validation on Delta — 2026-10-19

The [GPU-path evidence](nvmedia-probe/evidence/gpu-demo-2026-10-19.json) records
hashes, timing samples, clock observations, recording/decode counts, and checks.
The [earlier prototype evidence](nvmedia-probe/evidence/rust-demo-2026-10-19.json)
is retained for comparison.

- 30 full-entropy synthetic frames: GPU readback and software decode match input.
- 30 queued camera frames with CUDA-event reclamation: GPU readback and software
  decode match; reconstruction restores all 124,416,000 native UYVY bytes.
- Six-camera Copper recording at requested 15 fps: 223, 222, 222, 222, 223,
  and 219 frames in 15.003 seconds. All extracted frames decoded, every stream
  began with an IDR, and decoder error counts were zero.
- One camera: 30 fps in the capture/encode benchmark.
- Six concurrent capture/encode processes: 15.6–16.0 fps each; NVENC ran
  at its observed maximum 793.6 MHz. This isolates disk and networking from that
  measurement; it does not establish a universal encoder hardware limit.
