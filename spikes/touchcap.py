"""Read-only touch capture for the Kobo Clara BW. Prints axis ranges, then every
event from the touch and key devices with a timestamp, for a fixed time."""
import fcntl, os, select, struct, sys, time

DEVICES = {"/dev/input/event1": "touch", "/dev/input/event0": "keys", "/dev/input/event2": "power"}
EV = {0: "SYN", 1: "KEY", 2: "REL", 3: "ABS", 4: "MSC"}
ABS = {0: "X", 1: "Y", 24: "PRESSURE", 47: "MT_SLOT", 48: "MT_TOUCH_MAJOR", 49: "MT_TOUCH_MINOR",
       52: "MT_ORIENTATION", 53: "MT_X", 54: "MT_Y", 55: "MT_TOOL_TYPE", 57: "MT_TRACKING_ID",
       58: "MT_PRESSURE", 59: "MT_DISTANCE"}
SYN = {0: "REPORT", 2: "MT_REPORT", 3: "DROPPED"}
FMT = "llHHi"  # 32-bit ARM: timeval(long, long), type, code, value
SIZE = struct.calcsize(FMT)

def absinfo(fd, axis):
    request = 0x80000000 | (24 << 16) | (0x45 << 8) | (0x40 + axis)
    buf = fcntl.ioctl(fd, request, b"\0" * 24)
    return struct.unpack("6i", buf)  # value, min, max, fuzz, flat, resolution

seconds = float(sys.argv[1]) if len(sys.argv) > 1 else 300
fds = {}
for path, label in DEVICES.items():
    fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
    fds[fd] = label
    if label == "touch":
        for axis, name in ABS.items():
            try:
                print("absinfo %s value/min/max/fuzz/flat/res=%s" % (name, absinfo(fd, axis)))
            except OSError:
                pass
print("capturing for %.0fs" % seconds, flush=True)
end = time.monotonic() + seconds
while time.monotonic() < end:
    ready, _, _ = select.select(list(fds), [], [], 1.0)
    for fd in ready:
        data = os.read(fd, SIZE * 64)
        for i in range(0, len(data) - SIZE + 1, SIZE):
            sec, usec, kind, code, value = struct.unpack(FMT, data[i:i + SIZE])
            if kind == 3:
                name = ABS.get(code, str(code))
            elif kind == 0:
                name = SYN.get(code, str(code))
            else:
                name = str(code)
            print("%d.%06d %s %s %s %d" % (sec, usec, fds[fd], EV.get(kind, kind), name, value), flush=True)
print("done", flush=True)
