"""How neutral are the bulb's greys? RGB greys against the white LEDs, seen by the webcam.

Drives the bulb through the running service. No frames are saved.
"""
import asyncio
import sys
import time

import numpy as np

import rig
from chsmartbulb import client, service

SOCKET = service.default_socket_path()
LEVELS = (255, 192, 128, 96, 64, 48, 32, 24, 16, 8, 4, 2, 1)


def call(**request):
    reply = asyncio.run(client.call(SOCKET, request))
    if not reply["ok"]:
        sys.exit(f"service refused {request}: {reply['error']}")
    return reply


def show(r=0, g=0, b=0, w=0, settle=0.8):
    call(cmd="color", color=f"#{r:02x}{g:02x}{b:02x}{w:02x}", brightness=1.0)
    time.sleep(settle)


def linear(rgb):
    """Undo the camera's gamma (taken as 2.2) so that light adds up."""
    return (np.asarray(rgb, dtype=float) / 255.0) ** 2.2


before = call(cmd="status")
print("state before:", {k: before[k] for k in ("on", "color", "brightness", "effect", "native")})
cam = rig.Camera()
cam.roi = None
try:
    # exposure: the highest one that saturates neither kind of white
    chosen = 50
    for exposure in (50, 80, 120, 166, 250, 400):
        cam.lock_controls(exposure)
        worst = 0.0
        for kind in (dict(r=255, g=255, b=255), dict(w=255)):
            show(**kind)
            frame = cam.frames(4, 6).mean(axis=0)
            worst = max(worst, float((frame.max(axis=2) > 245).mean()))
        print(f"exposure {exposure}: saturated fraction {worst:.3f}")
        if worst < 0.01:
            chosen = exposure
    cam.lock_controls(chosen)
    print("chosen exposure:", chosen)

    # region: pixels that follow the bulb and never saturate
    shots = []
    for kind in (dict(), dict(r=255), dict(g=255), dict(b=255), dict(w=255), dict(r=255, g=255, b=255)):
        show(**kind)
        shots.append(cam.frames(6, 6).mean(axis=0))
    stack = np.stack(shots)
    change = (stack.max(axis=0) - stack.min(axis=0)).sum(axis=2)
    unsaturated = stack.max(axis=(0, 3)) < 245
    score = np.where(unsaturated, change, 0)
    roi = score >= max(np.quantile(score[unsaturated], 0.85), 30)
    cam.roi = roi
    print(f"region: {int(roi.sum())} of {roi.size} pixels")

    def probe(name, **kind):
        show(**kind)
        rgb, saturated = cam.measure(8, 6)
        return name, rgb, saturated

    rows = [probe("dark")]
    rows += [probe(name, **kind) for name, kind in (("R 255", dict(r=255)), ("G 255", dict(g=255)), ("B 255", dict(b=255)))]
    for level in LEVELS:
        rows.append(probe(f"RGB {level}", r=level, g=level, b=level))
        rows.append(probe(f"W {level}", w=level))
    for r, g, b in ((128, 128, 128), (255, 128, 128), (128, 255, 128), (128, 128, 255), (255, 255, 128)):
        grey = min(r, g, b)
        rows.append(probe(f"{r:02x}{g:02x}{b:02x} rgb", r=r, g=g, b=b))
        rows.append(probe(f"{r:02x}{g:02x}{b:02x} +w", r=r - grey, g=g - grey, b=b - grey, w=grey))
    rows.append(probe("dark again"))

    dark = linear(rows[0][1])
    print(f"{'stimulus':12s} {'camera R,G,B':>20s} {'sat':>5s} {'light (linear, dark removed)':>32s} {'share R/G/B':>16s}")
    for name, rgb, saturated in rows:
        light = np.clip(linear(rgb) - dark, 0, None)
        total = light.sum()
        share = "   -" if total < 2e-4 else "/".join(f"{v:.2f}" for v in light / total)
        print(f"{name:12s} {rgb!s:>20s} {saturated:5.2f} {'  '.join(f'{v:.5f}' for v in light):>32s} {share:>16s}")
finally:
    cam.close()
    call(cmd="color", color=before["color"], brightness=before["brightness"])
    if before["effect"]:
        call(cmd="effect", name=before["effect"]["name"], params=before["effect"]["params"] or {})
    elif before["native"]:
        call(cmd="native", **before["native"])
    if not before["on"]:
        call(cmd="off")
    after = call(cmd="status")
    print("state after:", {k: after[k] for k in ("on", "color", "brightness", "effect", "native")})
