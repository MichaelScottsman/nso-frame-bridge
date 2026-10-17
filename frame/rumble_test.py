"""Plays a 1 s rumble on the bridge's virtual Xbox 360 pad via evdev FF,
exercising the same path SDL/Steam use. Usage: python3 rumble_test.py [secs]"""
import fcntl, glob, os, struct, sys, time

secs = float(sys.argv[1]) if len(sys.argv) > 1 else 1.0
dev = None
for d in sorted(glob.glob("/sys/class/input/event*")):
    try:
        if open(d + "/device/name").read().strip() == "Microsoft X-Box 360 pad":
            dev = "/dev/input/" + os.path.basename(d)
    except OSError:
        pass
if not dev:
    sys.exit("bridge pad not found")

FF_RUMBLE, EV_FF = 0x50, 0x15
# struct ff_effect (64-bit): type, id, direction, trigger{button,interval},
# replay{length,delay}, pad to 16, union (rumble: strong, weak) padded to 32.
effect = struct.pack("<HhHHHHH2xHH28x", FF_RUMBLE, -1, 0, 0, 0, int(secs * 1000), 0, 0xC000, 0xC000)
EVIOCSFF = 0x40000000 | (len(effect) << 16) | (ord("E") << 8) | 0x80
fd = os.open(dev, os.O_RDWR)
buf = bytearray(effect)
fcntl.ioctl(fd, EVIOCSFF, buf)
eid = struct.unpack_from("<h", buf, 2)[0]
os.write(fd, struct.pack("<qqHHi", 0, 0, EV_FF, eid, 1))
print(f"rumbling {dev} effect {eid} for {secs}s")
time.sleep(secs + 0.2)
os.write(fd, struct.pack("<qqHHi", 0, 0, EV_FF, eid, 0))
os.close(fd)
