#!/usr/bin/env python3
"""Encode shared synthetic YUV444 frames with NvMedia IEP; verify decoded bytes."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import re
import shlex
import shutil
import subprocess
import time


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--bits", type=int, choices=range(1, 9), default=4)
    parser.add_argument("--frames", type=int, choices=range(1, 121), default=1)
    parser.add_argument("--size", choices=["1920x1080", "1280x720", "320x256"], default="1920x1080")
    parser.add_argument("--capacity-mib", type=int, choices=range(1, 257), default=64)
    parser.add_argument("--mode", choices=["lossless", "control"], default="lossless")
    parser.add_argument("--reserve-size", choices=["1920x1080", "1920x1536", "3840x2160"])
    parser.add_argument("--memory-optimization", action="store_true")
    args = parser.parse_args()
    for tool in ("cc", "gst-launch-1.0"):
        if not shutil.which(tool):
            parser.error(f"missing tool: {tool}")
    if os.environ.get("LD_PRELOAD"):
        parser.error("unset LD_PRELOAD before measuring library isolation")
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=False)
    source = Path(__file__).resolve().parent
    width, height = map(int, args.size.split("x"))
    initial_width, initial_height = map(int, (args.reserve_size or args.size).split("x"))
    if initial_width < width or initial_height < height:
        parser.error("reserve dimensions must cover active dimensions")
    frame = random.Random(0).randbytes(width * height * 3 * args.frames)
    if args.bits < 8:
        frame = frame.translate(bytes(16 + (value & ((1 << args.bits) - 1)) for value in range(256)))
    input_path = args.output / "input.y444"
    input_path.write_bytes(frame)
    binary = args.output / "probe"
    encoded = args.output / "encoded.h265"
    upload = args.output / "upload.y444"
    decoded = args.output / "decoded.y444"
    commands = [
        ["cc", "-std=c11", "-O2", "-g", "-Wall", "-Wextra", "-Werror", "-Wpedantic",
         str(source / "probe.c"), "-ldl", "-o", str(binary)],
        [str(binary), str(input_path), str(encoded), str(upload), str(width), str(height),
         str(args.capacity_mib * 1024 * 1024), args.mode, str(initial_width), str(initial_height),
         "1" if args.memory_optimization else "0", str(args.frames)],
        ["gst-launch-1.0", "-e", "-f", "-v", "filesrc", f"location={encoded}",
         "!", "h265parse", "!", "avdec_h265", "!", "video/x-raw,format=Y444",
         "!", "filesink", f"location={decoded}", "sync=false"],
    ]
    (args.output / "commands.sh").write_text("\n".join(shlex.join(cmd) for cmd in commands) + "\n")
    summary = {
        "mode": args.mode, "bits": args.bits, "width": width, "height": height,
        "initial_width": initial_width, "initial_height": initial_height,
        "memory_optimization": args.memory_optimization,
        "frames": args.frames, "capacity_bytes": args.capacity_mib * 1024 * 1024,
        "input_bytes": len(frame), "input_sha256": sha256(input_path),
        "source_sha256": {name: sha256(source / name) for name in ("probe.c", "iep_compat.h", "run.py")},
        "uname": list(os.uname()), "stages": [], "passed": False,
        "libraries_sha256": {name: sha256(Path("/usr/lib") / name) for name in
                             ("libnvmedia_iep_sci.so", "libnvscibuf.so", "libnvscisync.so", "libnvvideo.so")
                             if (Path("/usr/lib") / name).exists()},
    }
    env = dict(os.environ, GST_DEBUG_NO_COLOR="1", GST_DEBUG="2")
    try:
        for stage, command in zip(("build", "encode", "decode"), commands):
            started = time.monotonic()
            with (args.output / f"{stage}.log").open("w") as log:
                try:
                    result = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, env=env, timeout=40)
                    code = result.returncode
                except subprocess.TimeoutExpired:
                    code = "timeout"
            summary["stages"].append({"stage": stage, "returncode": code,
                                      "seconds": round(time.monotonic() - started, 3)})
            content = (args.output / f"{stage}.log").read_text(errors="replace")
            if stage == "encode":
                summary["frame_bytes"] = [int(value) for value in re.findall(r"^encoded_bytes=(\d+)", content, re.M)]
                summary["encoded_frames"] = len(summary["frame_bytes"])
                summary["api_calls"] = re.findall(r"^call=(.+) status=(0x[0-9a-f]+)$", content, re.M)
                summary["v4l2_or_gstreamer_loaded"] = (
                    "v4l2_or_gstreamer_loaded=true" in content
                    if "v4l2_or_gstreamer_loaded=" in content else None)
                summary["upload_exact"] = "upload_exact=true" in content
                summary["internal_buffer_error"] = "Allocated bitstream size is less than size required" in content
                failures = [call for call in summary["api_calls"] if call[1] != "0x00000000"]
                if failures:
                    summary["failed_api"] = failures[-1]
                version = re.search(r"iep_version=([\d.]+)", content)
                if version:
                    summary["iep_version"] = version[1]
                for field in ("available_bytes", "encoded_bytes", "bitstream_bytes"):
                    match = re.search(rf"\b{field}=(\d+)", content)
                    if match:
                        summary[field] = int(match[1])
                if code == 0 and ("upload_exact=true" not in content or "encode_passed=true" not in content
                                  or "v4l2_or_gstreamer_loaded=false" not in content):
                    raise RuntimeError("missing encode validation markers")
            if code != 0 or any(token in content for token in ("CRITICAL", "ERROR")):
                raise RuntimeError(f"{stage} failed; inspect {stage}.log")
        summary["upload_sha256"] = sha256(upload)
        summary["decoded_sha256"] = sha256(decoded)
        summary["decoded_bytes"] = decoded.stat().st_size
        summary["encoded_bytes"] = encoded.stat().st_size
        summary["passed"] = (upload.read_bytes() == frame and decoded.read_bytes() == frame)
        if not summary["passed"]:
            summary["error"] = "decoded bytes differ from input"
    except (RuntimeError, OSError) as error:
        summary["error"] = str(error)
    finally:
        (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(json.dumps(summary), flush=True)
    return 0 if summary["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
