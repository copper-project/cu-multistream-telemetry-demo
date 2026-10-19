# Read allocation arguments and the later capacity check in the probe's process.
# Offsets are valid only for the pinned Delta libnvvideo.so SHA-256 below.
set pagination off
set confirm off
set breakpoint pending on
set disable-randomization off
handle SIGPIPE nostop noprint pass
python
import gdb
import hashlib
import json
import os

expected = "ce4be41ad67a64dfea905693af15a89344fad2a7a50cc04f8850207032011e51"
with open("/usr/lib/libnvvideo.so", "rb") as library:
    actual = hashlib.sha256(library.read()).hexdigest()
if actual != expected:
    raise gdb.GdbError("libnvvideo.so hash mismatch; refusing stale offsets")


def reg(name):
    return int(gdb.parse_and_eval("$" + name))


def uint(address, length):
    data = gdb.selected_inferior().read_memory(address, length)
    return int.from_bytes(bytes(data), "little")


def emit(kind, **fields):
    gdb.write("IEP_TRACE " + json.dumps(dict(kind=kind, **fields)) + "\n")


capture_points = []


class CapacityCheck(gdb.Breakpoint):
    def stop(self):
        context = reg("x19")
        emit("capacity_check", capacity=uint(context + 0x1a140, 4),
             encoded_bytes=reg("w1"), width=uint(context + 0xd07c, 2),
             height=uint(context + 0xd07e, 2), offset="0x39f74")
        return False


class Allocation(gdb.Breakpoint):
    def __init__(self):
        super().__init__("NvVideoAllocMem", internal=True)
        self.check = None

    def stop(self):
        base = int(gdb.parse_and_eval("(void *) &NvVideoAllocMem")) - 0x9440
        # GDB may stop after PACIASP; strip the return-address signature.
        if (reg("x30") & ((1 << 48) - 1)) - base != 0x37728:
            return False
        context = reg("x28") - 0x4000
        emit("bitstream_allocation", size=reg("w2"), alignment=reg("w1"),
             width=uint(context + 8, 2), height=uint(context + 10, 2),
             memory_optimization=uint(context + 0x17885, 1),
             stored_capacity=uint(context + 0x1a140, 4), offset="0x37724")
        if self.check is None:
            self.check = CapacityCheck("*" + hex(base + 0x39f74), internal=True)
        return False


class CaptureFormat(gdb.Breakpoint):
    def __init__(self, address, phase):
        super().__init__("*" + hex(address), internal=True)
        self.phase = phase

    def stop(self):
        address = reg("x19")
        if uint(address, 4) == 9 and uint(address + 16, 4) == int.from_bytes(b"H265", "little"):
            emit("v4l2_capture_size", phase=self.phase, width=uint(address + 8, 4),
                 height=uint(address + 12, 4), sizeimage=uint(address + 28, 4))
            if self.phase == "after":
                # The direct probe dlcloses the library immediately afterward.
                for point in capture_points:
                    point.enabled = False
        return False


class WrapperEntry(gdb.Breakpoint):
    def __init__(self):
        super().__init__("TegraV4L2_Ioctl", internal=True)
        self.points = []

    def stop(self):
        if not self.points:
            base = int(gdb.parse_and_eval("(void *) &TegraV4L2_Ioctl")) - 0x29d0
            self.points = [CaptureFormat(base + 0xb48c, "before"), CaptureFormat(base + 0xb4ac, "after")]
            capture_points.extend(self.points)
        return False


Allocation()
if os.environ.get("TRACE_V4L2") == "1":
    with open("/usr/lib/libtegrav4l2.so", "rb") as library:
        if hashlib.sha256(library.read()).hexdigest() != "937160c2b910c4d2d48f6d338b101f1f8b4203b7e74c8f73696e2854c386c816":
            raise gdb.GdbError("libtegrav4l2.so hash mismatch")
    WrapperEntry()
gdb.events.exited.connect(lambda event: emit("process_exit", code=getattr(event, "exit_code", None)))
end
run
