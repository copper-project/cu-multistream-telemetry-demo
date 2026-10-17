# Delta compression benchmark — 2026-10-17

CPU compression is fast enough for six 1080p UYVY cameras at 15 fps on this
sample, provided encoding runs in parallel. Plane separation plus LZ4 is the
simplest candidate; reversible prediction plus Zstd level 1 saves substantially
more bandwidth at a higher CPU cost. The streaming application has not been
changed to use either codec.

| Layout / codec | Ratio | Payload at 6 × 15 fps | Encode capacity, fps/camera | Encode p95 | Decode capacity, fps/camera |
| --- | ---: | ---: | ---: | ---: | ---: |
| Packed UYVY / LZ4 | 1.40:1 | 2137 Mbit/s | 59.0 | 17.0 ms | 270.8 |
| Planar Y/U/V / LZ4 | 1.83:1 | 1629 Mbit/s | 56.7 | 17.6 ms | 242.0 |
| Planar Y/U/V / Zstd -3 | 2.00:1 | 1492 Mbit/s | 52.7 | 19.0 ms | 181.6 |
| Planar Y/U/V / Zstd 1 | 2.24:1 | 1331 Mbit/s | 38.6 | 26.0 ms | 118.5 |
| Planar + left prediction / Zstd 1 | 3.13:1 | 955 Mbit/s | 31.3 | 32.0 ms | 78.1 |

Capacity columns use six simultaneous workers, one per camera. They report
maximum offline processing capacity divided by six, not measured live capture
or display fps. The bandwidth column projects the measured compressed sizes
at the requested 15 fps. Encoding includes preprocessing; decoding includes
reconstructing the original packed UYVY bytes. Frame buffers and codec contexts
are reused. See [methodology](README.md) and [all measured cases](results/jetson-2026-10-17.csv).

One serial worker does not meet the 373.25 MB/s raw-input requirement:
planar LZ4 reached 255.5 MB/s, versus 1410.6 MB/s with six workers. Planar Zstd
level 1 reached 172.2 MB/s with one worker and 959.4 MB/s with six. Worker threads
are therefore required; these numbers do not measure CPU utilization in a
paced 15 fps application. Zstd level 3 missed the target in all three layouts.

Every one of the 180 sampled frames was encoded, decoded, and compared byte
for byte for every layout/codec combination before timing. Prediction is
reversible wrapping subtraction within each row; no camera samples are lost,
and frames do not depend on previous frames.

Hardware and workload:

- Delta / Tegra234, eight online CPU cores, stock 40 W power mode.
- `schedutil` governor; reported CPU maximum 1.984 GHz. Clocks/power limits were
  not changed. Observed CPU temperature was approximately 61–63 °C.
- Native LZ4 1.10.0 and Zstd 1.5.7, compiled in release mode through Cargo.
- Six different cameras captured concurrently in native UYVY, 1920×1080 at
  requested 30 fps; 40 frames each, first 10 discarded, 30 used per camera.
- Three timed passes through each sample set for each worker count/direction.
- Capture completed before benchmarking; no video encoder or GPU workload.

Raw six-camera input at 15 fps is 2985.984 Mbit/s. The benchmark uses a
2200 Mbit/s compressed-payload budget to leave room on the 2.5 Gbit/s link.
The reported rates exclude Copper headers, FEC, Ethernet overhead, and
serialization/capture costs. The final integration still needs a live test.

This is a short sample of the current scene. Different motion, exposure,
sensor noise, or camera content can change lossless compression ratios and
timings. The raw samples remain outside Git in `/tmp/compression-samples`
on Delta and disappear on reboot. The environment, capture commands, and raw
sample hashes are saved in the [environment manifest](results/jetson-2026-10-17.json);
images are not committed.
