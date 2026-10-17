# Native UYVY compression benchmark

This standalone workspace package benchmarks native LZ4 1.10 and Zstandard
1.5.7 via Rust bindings. Their C sources build with Cargo; system compression
libraries and Copper/GStreamer development libraries are not needed to build it.
It does not change the live streaming pipeline.

Measured Delta results are in [RESULTS.md](RESULTS.md).

On Delta, from `/root/cu-multistream-telemetry-bench`, after the cameras finish
initializing and with other camera applications stopped:

```bash
/data/dev/bin/dev-shell cargo +stable build -r --locked -p compression-bench
/data/dev/bin/dev-shell python3 compression-bench/capture.py /tmp/compression-samples
target/release/compression-bench /tmp/compression-samples/cam*.uyvy > /tmp/compression-results.csv
```

Use a fresh capture directory; existing directories are rejected. Capture uses
GStreamer to save 40 tightly packed 1920x1080 UYVY frames from each camera at
30 fps, concurrently, with no conversion or encoder. About 1 GB of temporary
storage is needed. `--devices /dev/cams/...` selects a camera subset; do not
substitute one camera six times when assessing six-scene compression ratios.

The benchmark skips the first 10 frames and preloads the remaining 30 per
camera. It compares packed UYVY, losslessly separated Y/U/V planes, and planar
samples with wrapping differences from the left neighbor (reset each row).
LZ4 default and Zstd levels -3, 1, and 3 encode every frame independently.
There is no inter-frame prediction, shared dictionary, chroma resampling, or
quantization. Every sampled frame is compressed, decompressed, and restored
byte for byte before timing begins; a mismatch fails the run.

Each case runs with one worker across all cameras, then one worker per camera.
Workers have separate codec contexts and reusable buffers. Timed encoding
includes preprocessing and compression; timed decoding includes decompression
and restoring packed UYVY. File I/O, allocation of frame buffers, initial
verification, and codec warmup are excluded. Three timed passes traverse the
samples in order. Throughput uses elapsed wall time, including worker joins;
p95 is measured separately for individual frame calls. Encode and decode are
timed separately, not simultaneously.

CSV columns report compression ratio, projected aggregate compressed payload
at 15 fps/camera, largest compressed frame, raw-input MB/s, effective
fps/camera, and per-frame p95 latency. `encode_realtime` means average encoder
capacity meets the requested frame rate. `payload_fits` compares the projection
to a conservative 2200 Mbit/s payload budget for the 2.5 Gbit/s connection.
Both must pass; also inspect p95 against the 66.7 ms frame period at 15 fps.
Protocol overhead is not included in the reported payload rate.

`--frames`, `--skip`, `--repeats`, `--fps`, and `--budget-mbps` change the test.
`--fps` changes only the bandwidth projection and required throughput, not the
contents or capture rate of the input files. All MB/GB units are decimal.

This measures CPU compression of a short captured scene with warm codec state,
not sustained live capture, Copper serialization/FEC, networking, or GPU work.
Noise, motion, exposure, temperature, power mode, and competing work can change
the outcome. Six workers deliberately consume substantial CPU capacity.

```bash
cargo test -p compression-bench
cargo clippy -p compression-bench --all-targets -- -D warnings
```
