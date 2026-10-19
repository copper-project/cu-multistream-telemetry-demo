# Delta encoder allocation analysis

The inspected H.265 paths calculate bitstream capacity from image dimensions
and a fixed ratio. They do not budget for lossless expansion. A larger initial
IEP resolution followed by a documented dynamic resolution change retains
larger buffers while producing native 1920×1080 video. Both tested reservation
sizes passed 30 full-range random frames with exact software-decoded bytes.

This identifies a library allocation limit behind the observed failures.
It does not establish the encoder hardware's maximum bitstream capacity.

## Allocation formulas

All addresses below are module-relative instruction offsets in Delta's pinned
binaries. These libraries are stripped; nearby exported symbol names in
`objdump` output are not reliable function names. The selected instructions are
in [allocation-disassembly.txt](evidence/allocation-disassembly.txt); binary
SHA-256 hashes, build IDs, test commands, logs, and debugger observations are in
[the evidence](evidence/allocation-analysis-2026-10-19.json).

### V4L2 capture buffer

On the traced 8-bit H.265 capture-format path, `libtegrav4l2.so+0xb48c..0xb4a8`
overwrites `sizeimage` with:

```text
max(32768, (width * height * 3) >> 1)
```

At 1920×1080, a requested 12,582,912 bytes becomes 3,110,400 bytes. The debugger
observed both values at the instructions surrounding the store. This path
ignores the caller's larger request; there is no size table in this calculation.

### IEP internal bitstream buffers

The H.265 creation path in `libnvvideo.so` rounds both initial dimensions up to
32 pixels, then counts 16×16 blocks (`+0x36abc..0x36ba4`). Allocation at
`+0x376bc..0x37724`, with its other branch at `+0x37980`, is equivalent to:

```text
W = align_up(initial_width, 32)
H = align_up(initial_height, 32)
blocks = (W / 16) * (H / 16)

if memory_optimization:
    capacity = align_up(max(32768, blocks * 384), 256)
else:
    capacity = max(32768, blocks * 768)

NvVideoAllocMem(..., alignment=256, size=capacity, ...)
```

For these rounded dimensions the capacities are respectively 1.5 and 3 bytes
per pixel. The public `disableMemoryOptimization=1` selects the larger branch;
the IEP wrapper inverts that flag at `libnvmedia_iep_sci.so+0xc52c..0xc5b4`.
Both branches were observed at runtime. Four bitstream allocation calls occurred
per tested encoder instance.

| Initial dimensions | Optimization | Rounded dimensions | Capacity per buffer |
| --- | --- | --- | ---: |
| 1920×1080 | Enabled | 1920×1088 | 3,133,440 |
| 1920×1080 | Disabled | 1920×1088 | 6,266,880 |
| 1920×1536 | Disabled | 1920×1536 | 8,847,360 |
| 3840×2160 | Disabled | 3840×2176 | 25,067,520 |

The capacity is stored at encoder-context offset `0x1a140`. At
`libnvvideo.so+0x39f74..0x39f80`, the completed encoded byte count is compared
against that field. Exceeding it branches to the diagnostic at `+0x3a090` and
returns native status 10, surfaced by IEP as status 7:

```text
tegraH265EncodeGetStatus: Allocated bitstream size is less than size required to encode a frame
```

A 64 MiB application buffer passed to `GetBits` does not affect these earlier
internal allocations. Native 1080p initialization fails when a 7-bit random
frame needs 6,653,814 bytes or an 8-bit random frame needs 8,248,045 bytes,
both above 6,266,880. These are all **8-bit YUV444 surfaces**; the random-bit
parameter controls sample entropy. The 7-bit samples are within 16..143.

## Tested application workaround

Use the existing public API in this order:

```text
Create H.265 IEP at larger initial dimensions, disableMemoryOptimization=1
SetAttribute(NvMediaEncSetAttr_DRCParams, {width=1920, height=1080})
SetConfiguration(lossless H.265 4:4:4)
Register and submit the actual 1920×1080 NvSciBuf image
```

The probe supplies matching attributes from a larger NvSciBuf object at
creation, keeps it alive until teardown, and submits a separate native 1080p
image. The larger image is never registered or encoded. The output is native
1080p, with no padding/cropping conversion in the decoder.

NVIDIA documents [dynamic resolution change through NvMediaIEPSetAttribute](https://developer.nvidia.com/docs/drive/drive-os/7.0.3/public/drive-os-linux-sdk-api-ref/nvmedia__iep__input__extradata_8h.html),
including calling it before the first frame. The reconstructed ABI uses
attribute type 0 and a four-byte struct containing two `uint16_t` dimensions.
The corresponding native update at `libnvvideo.so+0x3b374..0x3b478` updates
active geometry without rewriting the original bitstream capacity. Debugger
observations confirm active 1920×1080 dimensions and the retained capacity on
every completed frame.

**Call DRC before `SetConfiguration`.** An earlier ordering experiment that
configured first produced the initial larger output resolution despite a
successful DRC return status. The implemented order passes the exact byte and
decoded-dimension checks.

## Validation

Each case uses one encoder instance and a 64 MiB application output buffer.
All frames are IDR; samples come from a deterministic seeded PRNG.

| Initial size / active size | Random bits/sample | Frames | Result |
| --- | ---: | ---: | --- |
| 1080p / 1080p | 6 | 1 | Exact; 5,587,632 encoded bytes |
| 1080p / 1080p | 7 | 1 | Internal capacity error |
| 1080p / 1080p | 8 | 1 | Internal capacity error |
| 1080p / 1080p, optimization enabled | 4 | 1 | Internal capacity error; needs 3,637,739 bytes |
| 1920×1536 / 1080p | 8 | 30 | All decoded bytes exact |
| 3840×2160 / 1080p | 8 | 30 | All decoded bytes exact |

Both successful reservation runs produced identical elementary streams:
247,435,785 bytes total, with frame sizes from 8,245,367 to 8,249,805 bytes.
Input, NvSciBuf readback, and software-decoded output each contain 186,624,000
bytes with SHA-256:

```text
45df17b5b73c91aa7329e1d4f0c8aab017268d86983b0860e441cc6df08137d0
```

Successful debugger reruns produced the same encoded and uploaded hashes as
the ordinary runs. The debugger only reads allocation arguments and context
fields; no register values or data buffers were changed. Installed library
files were not modified.

Reproduce the larger-reservation case on Delta from this directory:

```bash
/data/dev/bin/dev-shell python3 run.py results/reserved-4k-30 --bits 8 --frames 30 --reserve-size 3840x2160
```

To inspect it with the pinned-library debugger script, use a new pair of output
paths and the binary/input generated by that run:

```bash
/data/dev/bin/dev-shell gdb --batch --nx -x trace-allocation.gdb --args results/reserved-4k-30/probe results/reserved-4k-30/input.y444 results/trace.h265 results/trace-upload.y444 1920 1080 67108864 lossless 3840 2160 0 30
```

This is a validated standalone workaround on this Delta image, with additional
memory cost. Thirty synthetic frames do not prove a worst-case size bound,
sustained frame rate, or six-camera concurrency. The 4K reservation offers
more tested headroom than 1920×1536. Copper integration and camera-load testing
remain separate work.
