"""Session requirements, command rate / latency, and volume field in the status frame."""
import subprocess
import time

import numpy as np

import rig

cam = rig.Camera()

# --- A: is the "01234567" hello required? -------------------------------------------------
link = rig.Link(hello=False)
r = link.query(0x02, "0000008000000080")
print("A1 no-hello query 51 02 ->", r.hex() if r else None)
link.light(b=255)
time.sleep(0.8)
print("A2 no-hello light blue -> cam", cam.measure(), "state", link.state())
link.light()
time.sleep(0.8)
dark = np.array(cam.measure()[0])
print("   dark", tuple(dark))
link.close("A no hello")

link = rig.Link()

# --- B: latency --------------------------------------------------------------------------
lat = []
for i in range(6):
    link.light()
    time.sleep(0.7)
    t0 = time.monotonic()
    link.light(r=255)
    while True:
        f = cam.frames(1, 0)[0]
        if f[cam.roi].mean(axis=0)[0] - dark[0] > 30:
            lat.append(time.monotonic() - t0)
            break
        if time.monotonic() - t0 > 2:
            lat.append(None)
            break
print("B latency send->visible (s, includes camera delay):", [round(x, 3) if x else None for x in lat])

# --- C: command rate ---------------------------------------------------------------------
for interval in (0.2, 0.1, 0.066, 0.04, 0.02, 0.01):
    n = int(2.0 / interval)
    obs = []
    stop = False
    t0 = time.monotonic()
    for i in range(n):
        if i % 2 == 0:
            link.light(r=255)
        else:
            link.light(b=255)
        target = t0 + (i + 1) * interval
        while time.monotonic() < target:
            f = cam.frames(1, 0)[0]
            v = f[cam.roi].mean(axis=0) - dark
            obs.append(1 if v[0] > v[2] else 0)
    sent_dur = time.monotonic() - t0
    # keep sampling a little to see whether a backlog is still playing out
    tail = []
    t1 = time.monotonic()
    while time.monotonic() - t1 < 1.0:
        f = cam.frames(1, 0)[0]
        v = f[cam.roi].mean(axis=0) - dark
        tail.append(1 if v[0] > v[2] else 0)
    flips = int(np.abs(np.diff(obs)).sum())
    tail_flips = int(np.abs(np.diff(tail)).sum())
    final = "red" if tail[-1] else "blue"
    expect = "red" if (n - 1) % 2 == 0 else "blue"
    print(f"C interval {interval * 1000:5.0f} ms: sent {n} in {sent_dur:.2f}s, camera saw {flips} flips during, "
          f"{tail_flips} flips after stop, final={final} (expected {expect}), state={link.state()}")
    time.sleep(0.5)

# --- D: idle then command (no keepalive) --------------------------------------------------
link.light(g=255)
time.sleep(0.6)
print("D before idle:", cam.measure())
time.sleep(25)
link.light(r=255)
time.sleep(0.6)
print("D after 25 s idle, set red:", cam.measure(), "state", link.state())

# --- E: status frame vs audio volume ------------------------------------------------------
def status():
    f = link.query(0x00, "0000008000000080")
    return f[16:].hex(" ") if f else None


def vol():
    return subprocess.run(["wpctl", "get-volume", "@DEFAULT_AUDIO_SINK@"], capture_output=True, text=True).stdout.strip()


print("E volume now:", vol())
print("E status    :", status())
orig_vol = vol().split()[1]
for v in ("0.30", "1.00", "0.00"):
    subprocess.run(["wpctl", "set-volume", "@DEFAULT_AUDIO_SINK@", v])
    time.sleep(1.2)
    print(f"E volume {v} -> status {status()}")
subprocess.run(["wpctl", "set-volume", "@DEFAULT_AUDIO_SINK@", orig_vol])
time.sleep(1.0)
print("E restored", vol(), "-> status", status())

link.light(w=255)
time.sleep(0.4)
print("restored light:", link.state())
cam.close()
link.close("session/rate/volume")
