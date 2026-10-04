"""Pick a camera exposure and a region of interest by toggling the bulb. No frames are saved."""
import json
import time

import numpy as np

import rig

link = rig.Link()
orig = link.state()
print("original state:", orig)
cam = rig.Camera(exposure=166)
cam.roi = None

# 1) exposure: full white, find the highest exposure with < 2% saturated pixels in the frame
link.light(w=255)
time.sleep(1.0)
chosen = 50
for exp in (50, 80, 120, 166, 250, 400, 800):
    cam.lock_controls(exp)
    time.sleep(0.8)
    f = cam.frames(4, 6).mean(axis=0)
    sat = float((f.max(axis=2) > 250).mean())
    print(f"exposure {exp}: mean={f.mean():.1f} saturated={sat:.3f}")
    if sat < 0.02:
        chosen = exp
cam.lock_controls(chosen)
time.sleep(0.8)
print("chosen exposure:", chosen)

# 2) ROI: pixels that change most between two settings and never saturate
shots = {}
for name, kw in (("red", dict(r=255)), ("green", dict(g=255)), ("blue", dict(b=255)), ("white", dict(w=255)), ("dark", dict())):
    link.light(**kw)
    time.sleep(1.0)
    shots[name] = cam.frames(6, 6).mean(axis=0)
    print(f"{name:6s} frame mean RGB = {shots[name].reshape(-1, 3).mean(axis=0).round(1)}  state={link.state()}")

stack = np.stack(list(shots.values()))
change = (stack.max(axis=0) - stack.min(axis=0)).sum(axis=2)
unsat = stack.max(axis=(0, 3)) < 245
score = np.where(unsat, change, 0)
thr = np.quantile(score[unsat], 0.85) if unsat.any() else 0
roi = score >= max(thr, 30)
print(f"ROI pixels: {int(roi.sum())} / {roi.size}; change threshold {thr:.1f}; max change {change.max():.1f}")
ys, xs = np.nonzero(roi)
if len(xs):
    print(f"ROI bbox x {xs.min()}..{xs.max()} y {ys.min()}..{ys.max()} (frame {rig.W}x{rig.H})")
np.save(rig.ROI_FILE, roi)
json.dump({"exposure": chosen}, open(rig.CAM_FILE, "w"))
cam.roi = roi
for name, kw in (("red", dict(r=255)), ("green", dict(g=255)), ("blue", dict(b=255)), ("white", dict(w=255)), ("yellow", dict(y=255)), ("dark", dict())):
    link.light(**kw)
    time.sleep(1.0)
    print(f"ROI {name:6s} -> {cam.measure()}")

# restore
g, b, r, _, _, w, y, _ = bytes.fromhex(orig)
link.light(g=g, b=b, r=r, w=w, y=y)
time.sleep(0.5)
print("restored state:", link.state())
cam.close()
link.close("calibration")
