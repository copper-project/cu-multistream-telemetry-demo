# Shared Jetson camera demo

Six UYVY cameras, six shared CUDA preparation tasks, and twelve independent HEVC
encoders: lossless recording plus 8 Mbps CBR preview for each camera. Both consumers
read the same Copper GPU handle. Native components live in the sibling current
Copper checkout's [`cu-jetson-camera`](../../copper-rs/components/sources/cu_jetson_camera)
crate, on branch `experiment/jetson-camera-components`.

Run on Delta, from the telemetry-demo repository:

```bash
just jetson 10 /tmp/jetson-six.copper
```

Arguments are duration in seconds and log path. All twelve encoder branches default
to 10 fps; one shared NVENC engine serves all sessions. An optional third argument,
`--no-log`, runs the same graph without recording encoded messages. This is a separate Rust workspace:
it builds without the original demo's GStreamer dependencies. It requires the
installed NVIDIA CUDA/NVRTC, NvSciBuf/NvSciSync, and NvMedia IEP runtime libraries.
The existing telemetry binaries and payload APIs are unchanged.

Edit [copperconfig.ron](copperconfig.ron) for camera paths, rates, compression, and
camera count. Remove/repeat a camera's source, preparation, encoder, and sink nodes
together with their connections. Both modes currently emit HEVC 4:4:4. Lossy
preview encoding shares hardware capacity with lossless recording.

Only encoded messages enter the local Copper log. Raw pixels remain in DMA/GPU
memory: one GPU UYVY→NV24 conversion is shared by both consumers. Compressed bytes
are retrieved into reusable host buffers and serialized by Copper's asynchronous logger.
Independent bounded workers can skip different raw frame sequences under overload;
use capture sequence and `tov` to correlate the two streams.

The local log intentionally contains both encoded branches. Its section size is
192 MiB to accommodate the configured worst-case access-unit capacities across all
twelve encoders. Increase the log section bound if increasing output capacities.
For a smaller one-camera graph, byte-exact validation, ownership details, and runtime
requirements, see the component's README and `copper`/`fanout` examples.

One-camera validation on Delta reached approximately 15 fps for simultaneous
lossless and 8 Mbps CBR encoding, and the decoded lossless frame matched all
6,220,800 expanded camera bytes. See the component's `VALIDATION.md` for scope and
hardware details.

The final 10-second six-camera run recorded 101–103 access units per branch with
10 fps requested. At a 15 fps request, both local recording and a repeat without
logging sustained approximately 10.5 fps per branch across all twelve sessions.
These are short runs; one repeated startup left a camera inactive until reopened.
The source now reports a two-second frame timeout explicitly.
