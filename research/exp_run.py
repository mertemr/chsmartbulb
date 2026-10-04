"""Run a list of light() experiments, printing camera delta-vs-dark for each.

Usage: python3 exp_run.py NOTE 'g=0,r=255' 'w=128' ...   (hex allowed: effect=0x52)
Special steps: 'sleep=2' waits; 'sample=N,dt=0.3' takes N extra measurements without sending.
"""
import sys
import time

import rig

note, steps = sys.argv[1], sys.argv[2:]
link = rig.Link()
orig = link.state()
cam = rig.Camera()
settle = 0.9


def meas():
    (r, g, b), sat = cam.measure()
    return r, g, b, sat


link.light()
time.sleep(settle)
dark = meas()
print(f"dark baseline RGB={dark[:3]}")
print(f"{'step':42s} {'dR':>7s} {'dG':>7s} {'dB':>7s}  sat  readback")
for step in steps:
    kw = {k: int(v, 0) if not v.replace('.', '').isdigit() or '.' not in v else float(v)
          for k, v in (p.split("=") for p in step.split(","))}
    if "sleep" in kw:
        time.sleep(kw["sleep"])
        continue
    if "sample" in kw:
        for i in range(int(kw["sample"])):
            time.sleep(float(kw.get("dt", 0)))
            r, g, b, sat = cam.measure(n=2, skip=1)
            print(f"{'  sample ' + str(i):42s} {r - dark[0]:7.1f} {g - dark[1]:7.1f} {b - dark[2]:7.1f}  {sat}")
        continue
    wait = kw.pop("wait", settle)
    link.light(**kw)
    time.sleep(wait)
    r, g, b, sat = meas()
    print(f"{step:42s} {r - dark[0]:7.1f} {g - dark[1]:7.1f} {b - dark[2]:7.1f}  {sat}  {link.state()}")

g, b, r, _, _, w, y, _ = bytes.fromhex(orig)
link.light(g=g, b=b, r=r, w=w, y=y)
time.sleep(0.4)
print("restored:", link.state())
cam.close()
link.close(note)
