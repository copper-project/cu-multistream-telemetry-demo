#!/usr/bin/env python3
"""Capture a short native UYVY sample from all six cameras concurrently."""
import argparse
import json
from pathlib import Path
import subprocess
import time

CAMERAS = [
    "/dev/cams/des0/port0/sensor0",
    "/dev/cams/des0/port1/sensor0",
    "/dev/cams/des0/port1/sensor1",
    "/dev/cams/des0/port1/sensor2",
    "/dev/cams/des1/port0/sensor0",
    "/dev/cams/des1/port0/sensor1",
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--frames", type=int, default=40)
    parser.add_argument("--devices", nargs="+", default=CAMERAS,
                        help="Explicit camera subset; default: all six kit cameras")
    args = parser.parse_args()
    if args.frames < 1:
        parser.error("--frames must be positive")
    missing = [device for device in args.devices if not Path(device).exists()]
    if missing:
        parser.error("missing camera devices: " + ", ".join(missing))
    args.directory.mkdir(parents=True, exist_ok=False)
    jobs = []
    metadata = {"frames": args.frames, "width": 1920, "height": 1080,
                "format": "UYVY", "started_unix": time.time(), "cameras": []}
    try:
        for i, device in enumerate(args.devices):
            path = args.directory / f"cam{i}.uyvy"
            command = ["gst-launch-1.0", "-e", "v4l2src", f"device={device}",
                       f"num-buffers={args.frames}", "!",
                       "video/x-raw,format=UYVY,width=1920,height=1080,framerate=30/1",
                       "!", "filesink", f"location={path}"]
            log = (args.directory / f"cam{i}.log").open("w")
            jobs.append((subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT), log))
            metadata["cameras"].append({"device": device, "file": str(path), "command": command})
        for i, (process, _) in enumerate(jobs):
            code = process.wait(timeout=30)
            size = (args.directory / f"cam{i}.uyvy").stat().st_size
            if code != 0 or size != args.frames * 1920 * 1080 * 2:
                raise RuntimeError(f"camera {i}: exit={code}, bytes={size}; see capture log")
            print(f"camera {i}: {args.frames} frames, {size} bytes", flush=True)
    finally:
        for process, log in jobs:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            log.close()
        metadata["elapsed_seconds"] = time.time() - metadata["started_unix"]
        (args.directory / "capture.json").write_text(json.dumps(metadata, indent=2) + "\n")


if __name__ == "__main__":
    main()
