"""Sample the light as a time series after each command.

Usage: python3 exp_fx.py NOTE SECONDS 'effect=0x52,speed=0x82,r=255' ...
Prints per-step stats (min/max per channel, estimated period) and a coarse trace.
"""
import sys
import time

import numpy as np

import rig

note, secs, steps = sys.argv[1], float(sys.argv[2]), sys.argv[3:]
link = rig.Link()
orig = link.state()
cam = rig.Camera()

link.light()
time.sleep(0.9)
dark = np.array(cam.measure()[0])
print(f"dark baseline RGB={tuple(dark)}")
BARS = " .:-=+*#%@"


def trace(v, lo, hi):
    if hi - lo < 4:
        return BARS[0] * len(v) if hi < 6 else BARS[5] * len(v)
    return "".join(BARS[min(9, int((x - lo) / (hi - lo) * 9.99))] for x in v)


for step in steps:
    kw = {k: int(v, 0) for k, v in (p.split("=") for p in step.split(","))}
    if kw.pop("pre", 0):
        link.light(b=255)
        time.sleep(1.0)
    link.light(**kw)
    t0 = time.monotonic()
    ts, vals = [], []
    while time.monotonic() - t0 < secs:
        f = cam.frames(1, 0)[0]
        ts.append(time.monotonic() - t0)
        vals.append(f[cam.roi].mean(axis=0) - dark)
    vals = np.array(vals)
    ts = np.array(ts)
    lum = vals.sum(axis=1)
    # period estimate from autocorrelation of luminance + hue-ish signal
    period = None
    sig = vals - vals.mean(axis=0)
    if sig.std() > 2:
        dt = float(np.median(np.diff(ts)))
        ac = sum(np.correlate(sig[:, c], sig[:, c], "full")[len(sig) - 1:] for c in range(3))
        ac = ac / ac[0]
        below = np.nonzero(ac < 0.2)[0]
        if len(below):
            k = below[0]
            peak = k + int(np.argmax(ac[k:]))
            if ac[peak] > 0.35 and peak < len(ac) - 2:
                period = round(peak * dt, 2)
    mn, mx = vals.min(axis=0), vals.max(axis=0)
    print(f"\n== {step}  (readback {link.state()})")
    print(f"   R {mn[0]:6.1f}..{mx[0]:6.1f}   G {mn[1]:6.1f}..{mx[1]:6.1f}   B {mn[2]:6.1f}..{mx[2]:6.1f}   n={len(ts)} period~{period}s")
    step_i = max(1, len(ts) // 100)
    for c, name in enumerate("RGB"):
        print(f"   {name} |{trace(vals[::step_i, c], min(0, mn.min()), max(mx.max(), 8))}|")

g, b, r, _, _, w, y, _ = bytes.fromhex(orig)
link.light(g=g, b=b, r=r, w=w, y=y)
time.sleep(0.4)
print("\nrestored:", link.state())
cam.close()
link.close(note)
