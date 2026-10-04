"""Measure effect period vs speed byte using rising mid-level crossings of the red channel."""
import time
import numpy as np
import rig

link = rig.Link(); cam = rig.Camera()
link.light(); time.sleep(0.9)
dark = np.array(cam.measure()[0])

def period(effect, speed, secs=8.0):
    link.light(r=255, effect=effect, speed=speed)
    time.sleep(0.3)
    t0 = time.monotonic(); ts = []; v = []
    while time.monotonic() - t0 < secs:
        f = cam.frames(1, 0)[0]
        ts.append(time.monotonic() - t0); v.append(f[cam.roi].mean(axis=0)[0] - dark[0])
    ts = np.array(ts); v = np.array(v)
    mid = (v.min() + v.max()) / 2
    up = np.nonzero((v[:-1] < mid) & (v[1:] >= mid))[0]
    if len(up) < 3:
        return None, len(up), float(v.max() - v.min())
    d = np.diff(ts[up])
    return float(np.median(d)), len(up), float(v.max() - v.min())

def pack(i):
    return ((i >> 2) & 3) | (i << 4) & 0xFF

print("breathing 0x52, app levels:")
for i in range(0, 16):
    p, n, amp = period(0x52, pack(i), 7.0)
    print(f"  level {i:2d} byte 0x{pack(i):02x}: period {p if p is None else round(p, 2)} s  (cycles {n}, amp {amp:.0f})", flush=True)
print("breathing 0x52, raw bytes:")
for sp in (0x80, 0x81, 0x82, 0x83, 0x84, 0x88, 0x8c, 0x03, 0x0f, 0xff):
    p, n, amp = period(0x52, sp, 7.0)
    print(f"  byte 0x{sp:02x}: period {p if p is None else round(p, 2)} s  (cycles {n})", flush=True)
print("flash 0x54, app levels:")
for i in (0, 3, 6, 9, 12, 15):
    p, n, amp = period(0x54, pack(i), 5.0)
    print(f"  level {i:2d} byte 0x{pack(i):02x}: period {p if p is None else round(p, 2)} s  (cycles {n})", flush=True)
print("heartbeat 0x56 (high nibble only):")
for i in (0, 5, 10, 15):
    p, n, amp = period(0x56, (i << 4) & 0xFF, 6.0)
    print(f"  level {i:2d} byte 0x{(i << 4) & 0xFF:02x}: period {p if p is None else round(p, 2)} s  (cycles {n})", flush=True)
link.light(w=255); time.sleep(0.4); print("restored", link.state())
cam.close(); link.close("speed table")
