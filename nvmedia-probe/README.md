# NvMedia IEP lossless probe

The reconstructed declarations work on Delta: a shared NvSciBuf image can be
encoded as lossless H.265 4:4:4 and retrieved without loading V4L2 or GStreamer
in the encoder process. At 1080p, the probe retrieves a 5,587,632-byte frame and
software-decodes all 6,220,800 original bytes exactly. Disassembly identified
the internal buffer's fixed size formula. Creating at a larger resolution,
then changing to 1080p before configuration, passed 30 full-range random frames
with exact decoded bytes. See [allocation analysis](ALLOCATION.md) for the
instructions, formulas, traces, and tested workaround.

## Run on Delta

Copy this directory to `/root/nvmedia-probe`, then run:

```bash
cd /root/nvmedia-probe
/data/dev/bin/dev-shell python3 run.py results/my-4bit
/data/dev/bin/dev-shell python3 run.py results/my-6bit --bits 6
/data/dev/bin/dev-shell python3 run.py results/my-7bit --bits 7
/data/dev/bin/dev-shell python3 run.py results/reserved-4k-30 --bits 8 --frames 30 --reserve-size 3840x2160
```

Each output directory must be new. The runner uses the existing native C
compiler, Python standard library, NVIDIA libraries, and GStreamer software
decoder (`h265parse`, `avdec_h265`). No NVIDIA SDK headers are required.

`--bits 1..7` generates deterministic samples in `16..16+2^bits-1`; `--bits 8`
uses the full byte range. Input is tightly packed planar YUV444. Each run
submits one IDR frame at 1920×1080 by default, with a nominal rate of 15 fps.
`--frames 1..120` reuses one encoder for multiple independent random frames.
`--size 1280x720` and `--size 320x256` select smaller frames.

`--reserve-size 1920x1536` or `--reserve-size 3840x2160` creates with larger
initial dimensions, then uses DRC before configuration to encode at `--size`.
`--memory-optimization` selects smaller internal buffers; it is disabled by
default. All surface formats remain 8-bit YUV444 regardless of `--bits`.

`--capacity-mib` controls the caller's output buffer (64 MiB by default).
`--mode control` disables lossless encoding and uses QP 20. The control should
encode and decode successfully but fail the byte comparison. A 1 MiB output
buffer should be rejected after `BitsAvailable`, before `GetBits` is called.

Results include source/library SHA-256 hashes, commands, API statuses, loaded
libraries, subprocess timings, and input/upload/decoded hashes. The runner exits
successfully only when all decoded frames match exactly. It preserves input,
upload, elementary stream, decoded frames, and binary in the output directory, including
on failure.

## Probe sequence

1. Load `libnvmedia_iep_sci.so`, `libnvscibuf.so`, and `libnvscisync.so` with
   `dlopen`. Require IEP 1.1.0 and positive NvSciBuf 2.11 / NvSciSync 2.8
   compatibility checks.
2. Reserve the caller's output buffer between two inaccessible guard pages.
3. Reconcile IEP and CPU requirements for a block-linear, semiplanar, 8-bit
   YUV444 image. Upload planar pixels with `NvSciBufObjPutPixels`, honor the
   cache-coherency attribute, and read them back for an exact comparison.
4. Create HEVC range-extensions encoding with lossless compression, constant
   QP 0, no B frames, and `disableMemoryOptimization=1` requested. Register the
   image and separate CPU-signaled pre-fence / CPU-waited EOF synchronization
   objects.
   With a larger reservation, change to the active resolution before configuring.
5. Feed each frame, wait for EOF, query `NvMediaIEPBitsAvailable`, check capacity,
   and retrieve `NvMediaIEPGetBits` into the caller's allocation.
6. Run a separate `gst-launch-1.0` process with `avdec_h265`, directly requesting
   planar Y444 without color conversion. Compare every decoded byte.

On this runtime, `GetBits` reports the correct byte count through its `numBytes`
argument but leaves `NvMediaBitstreamBuffer.bitstreamBytes` at zero. The probe
uses `numBytes`, requires it to match `BitsAvailable`, and checks any nonzero
`bitstreamBytes` value for consistency.

## Delta measurements with native-size initialization — 2026-10-19

All cases below use one frame and a 64 MiB caller buffer, except the explicit
small-buffer case. Machine: Yaak Delta / Tegra234, EOS 2.0-rc5,
L4T R39.2.1. [Evidence](evidence/delta-2026-10-19.json) preserves all eight
summaries, commands, and encode/decode logs. Full artifacts remain on Delta
under `/root/nvmedia-probe/results/delta-*`.

| Case | Encoded bytes | Result |
| --- | ---: | --- |
| 1080p, 4 bits/sample | 3,637,739 | Exact upload and decoded bytes |
| 1080p, 5 bits/sample | 4,584,918 | Exact upload and decoded bytes |
| 1080p, 6 bits/sample | 5,587,632 | Exact upload and decoded bytes |
| 1080p, 7 bits/sample | — | Internal buffer error |
| 1080p, 8 bits/sample | — | Internal buffer error |
| 720p, 8 bits/sample | — | Internal buffer error |
| 1080p, QP 20 control | 1,184,328 | Byte mismatch detected |
| 1080p, 4 bits/sample, 1 MiB caller buffer | 3,637,739 available | Rejected before `GetBits` |

The 7-bit case remains within nominal video sample ranges (16..143).
It fails at `BitsAvailable` with `NVMEDIA_STATUS_ERROR` (7), after successful
submission and EOF, accompanied by this driver diagnostic:

```text
tegraH265EncodeGetStatus: Allocated bitstream size is less than size required to encode a frame
```

The successful cases exceed the V4L2 path's measured 3,110,400-byte capture
allocation. IEP therefore avoids that particular clamp, but caller allocation
does not control all internal encoder buffering. Subsequent disassembly and
debugger inspection established that capacity and the reservation workaround;
see [allocation analysis](ALLOCATION.md). These initial runs do not establish
sustained frame rate or six-stream behavior.
Subprocess timings include initialization and teardown and are not throughput
measurements. The encoder process loads `libnvvideo.so`; its mapped libraries
contain no V4L2 or GStreamer libraries. GStreamer is used only by the separate
software-decoder process.

The normal demo's Rust graph and pipeline are unchanged by this experiment.

## Declaration sources

`iep_compat.h` independently expresses only the types, constants, and signatures
needed by this probe. Opaque objects stay opaque; the public structs use the
documented scalar widths, complete field ordering, and native C bitfields.
Compile-time assertions check LP64 layout. Runtime version checks and exact
roundtrips establish the tested subset on this Delta image; they do not prove
compatibility with every library reporting the same version.

Public NVIDIA DRIVE OS 7.0.3 API references:

- [IEP 1.1.0 signatures](https://developer.nvidia.com/docs/drive/drive-os/7.0.3/public/drive-os-linux-sdk-api-ref/nvmedia__iep_8h_source.html)
- [Encoder structs and feature flags](https://developer.nvidia.com/docs/drive/drive-os/7.0.3/public/drive-os-linux-sdk-api-ref/nvmedia__common__encode_8h_source.html)
- [Dynamic resolution changes](https://developer.nvidia.com/docs/drive/drive-os/7.0.3/public/drive-os-linux-sdk-api-ref/nvmedia__iep__input__extradata_8h.html)
- [Bitstream buffer](https://developer.nvidia.com/docs/drive/drive-os/7.0.3/public/drive-os-linux-sdk-api-ref/nvmedia__common__encode__decode_8h_source.html)
- [Core types and statuses](https://developer.nvidia.com/docs/drive/drive-os/7.0.3/public/drive-os-linux-sdk-api-ref/nvmedia__core_8h_source.html)
- [NvSciBuf declarations](https://developer.nvidia.com/docs/drive/drive-os/7.0.3/public/drive-os-linux-sdk-api-ref/nvscibuf_8h_source.html)
- [Attribute value types](https://developer.nvidia.com/docs/drive/drive-os/7.0.3/public/drive-os-linux-sdk-api-ref/group__nvscibuf__attr__key.html)
- [Pixel upload/readback contracts](https://developer.nvidia.com/docs/drive/drive-os/7.0.3/public/drive-os-linux-sdk-api-ref/group__nvscibuf__obj__api.html)
- [NvSciSync declarations](https://developer.nvidia.com/docs/drive/drive-os/7.0.3/public/drive-os-linux-sdk-api-ref/nvscisync_8h_source.html)

Use matching vendor headers when available for a maintained integration.
