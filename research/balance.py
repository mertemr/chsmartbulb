"""Which red and blue values, next to full green, look like the white LEDs to the webcam?

Closed loop: a candidate mix is compared with the white LEDs at the level that gives the same
camera green, so no assumption about the camera's gamma is needed. Drives the bulb through the
running service. No frames are saved.
"""
import asyncio
import sys
import time

import numpy as np

import rig
from chsmartbulb import Color, client, screen, service

SOCKET = service.default_socket_path()
WHITE_LEVELS = (16, 24, 32, 48, 64, 96, 128, 160, 192, 224, 255)


def call(**request):
    reply = asyncio.run(client.call(SOCKET, request))
    if not reply["ok"]:
        sys.exit(f"service refused {request}: {reply['error']}")
    return reply


def show(r=0, g=0, b=0, w=0, settle=0.8):
    call(cmd="color", color=f"#{r:02x}{g:02x}{b:02x}{w:02x}", brightness=1.0)
    time.sleep(settle)


before = call(cmd="status")
print("state before:", {k: before[k] for k in ("on", "color", "brightness", "effect", "native")})
cam = rig.Camera()
cam.roi = None
try:
    chosen = 50
    for exposure in (50, 80, 120, 166, 250):
        cam.lock_controls(exposure)
        show(w=255)
        frame = cam.frames(4, 6).mean(axis=0)
        saturated = float((frame.max(axis=2) > 245).mean())
        print(f"exposure {exposure}: saturated fraction {saturated:.3f}")
        if saturated < 0.01:
            chosen = exposure
    cam.lock_controls(chosen)
    print("chosen exposure:", chosen)

    shots = []
    for kind in (dict(), dict(g=255), dict(w=255), dict(w=64)):
        show(**kind)
        shots.append(cam.frames(6, 6).mean(axis=0))
    stack = np.stack(shots)
    change = (stack.max(axis=0) - stack.min(axis=0)).sum(axis=2)
    unsaturated = stack.max(axis=(0, 3)) < 245
    score = np.where(unsaturated, change, 0)
    cam.roi = score >= max(np.quantile(score[unsaturated], 0.85), 30)
    print(f"region: {int(cam.roi.sum())} of {cam.roi.size} pixels")

    def camera(**kind):
        show(**kind)
        return np.array(cam.measure(8, 6)[0])

    dark = camera()
    print("dark:", dark)
    white = np.array([camera(w=level) - dark for level in WHITE_LEVELS])
    for level, seen in zip(WHITE_LEVELS, white):
        print(f"white {level:3d}: camera {seen.round(1)}")

    def white_like(green):
        """What the camera shows for the white LEDs at the level with this much camera green."""
        return np.array([np.interp(green, white[:, 1], white[:, channel]) for channel in range(3)])

    def compare(r, g, b):
        seen = camera(r=r, g=g, b=b) - dark
        wanted = white_like(seen[1])
        return seen, wanted

    r, b = 40.0, 23.0
    for attempt in range(16):
        seen, wanted = compare(round(r), 255, round(b))
        off = seen / wanted
        print(f"try {attempt}: r={round(r):3d} g=255 b={round(b):3d} camera {seen.round(1)} white at that green {wanted.round(1)} "
              f"red x{off[0]:.2f} blue x{off[2]:.2f}")
        if abs(off[0] - 1) < 0.03 and abs(off[2] - 1) < 0.03:
            break
        r = min(255.0, max(1.0, r / off[0] ** 0.5))
        b = min(255.0, max(1.0, b / off[2] ** 0.5))
    gains = (round(r) / 255, round(b) / 255)
    print(f"gains next to green: red {gains[0]:.3f}, blue {gains[1]:.3f}")

    print("the same gains at lower levels, and the plain mix for comparison:")
    for g in (255, 192, 128, 64):
        for name, mix in (("balanced", (round(g * gains[0]), g, round(g * gains[1]))), ("plain", (g, g, g))):
            seen, wanted = compare(*mix)
            off = seen / wanted
            print(f"  {name:8s} {mix!s:16s} camera {seen.round(1)!s:20s} red x{off[0]:.2f} blue x{off[2]:.2f} of white")

    print("what the library's screen.balanced() makes of grey:")
    for level in (255, 128, 64):
        mix = screen.balanced(Color(level, level, level))
        seen, wanted = compare(mix.r, mix.g, mix.b)
        off = seen / wanted
        print(f"  ({mix.r}, {mix.g}, {mix.b}) camera {seen.round(1)!s:20s} red x{off[0]:.2f} blue x{off[2]:.2f} of white")

    print("mixed colours, plain and balanced (strongest channel kept):")
    for r0, g0, b0 in ((255, 255, 0), (255, 128, 0), (0, 255, 255), (128, 255, 0), (255, 0, 255)):
        mixed = np.array([r0 * gains[0], g0, b0 * gains[1]])
        balanced = np.round(mixed * max(r0, g0, b0) / mixed.max()).astype(int)
        for name, mix in (("plain", (r0, g0, b0)), ("balanced", tuple(int(v) for v in balanced))):
            seen = camera(r=mix[0], g=mix[1], b=mix[2]) - dark
            share = "/".join(f"{v:.2f}" for v in seen / seen.sum())
            print(f"  {r0:02x}{g0:02x}{b0:02x} {name:8s} {mix!s:16s} camera {seen.round(1)!s:20s} share {share}")
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
