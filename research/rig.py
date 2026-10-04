"""Measurement rig: bulb SPP link + webcam colour probe (frames are never written to disk)."""
import json
import os
import socket
import subprocess
import threading
import time

import numpy as np

ADDR = "AA:BB:CC:DD:EE:FF"
CHANNEL = 2
MAGIC = bytes.fromhex("01fe0000")
W, H = 160, 90
HERE = os.path.dirname(os.path.abspath(__file__))
ROI_FILE = os.path.join(HERE, "roi.npy")
CAM_FILE = os.path.join(HERE, "cam.json")
LOG = os.path.join(HERE, "exp.jsonl")


def v4l2(**ctrls):
    arg = ",".join(f"{k}={v}" for k, v in ctrls.items())
    subprocess.run(["v4l2-ctl", "-d", "/dev/video0", "-c", arg], check=False,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


class Camera:
    def __init__(self, exposure=None):
        if exposure is None and os.path.exists(CAM_FILE):
            exposure = json.load(open(CAM_FILE))["exposure"]
        self.exposure = exposure or 166
        self.proc = subprocess.Popen(
            ["ffmpeg", "-loglevel", "error", "-f", "v4l2", "-input_format", "yuyv422",
             "-video_size", "640x360", "-framerate", "30", "-i", "/dev/video0",
             "-vf", f"scale={W}:{H}", "-pix_fmt", "rgb24", "-f", "rawvideo", "-"],
            stdout=subprocess.PIPE)
        self.frame = None
        self.count = 0
        self.cond = threading.Condition()
        self.alive = True
        threading.Thread(target=self._run, daemon=True).start()
        time.sleep(0.3)
        self.lock_controls(self.exposure)
        self.roi = np.load(ROI_FILE) if os.path.exists(ROI_FILE) else None

    def lock_controls(self, exposure):
        self.exposure = exposure
        v4l2(white_balance_automatic=0)
        v4l2(white_balance_temperature=4500)
        v4l2(auto_exposure=1)
        v4l2(exposure_dynamic_framerate=0)
        v4l2(exposure_time_absolute=exposure)

    def _run(self):
        n = W * H * 3
        while self.alive:
            buf = self.proc.stdout.read(n)
            if len(buf) < n:
                break
            with self.cond:
                self.frame = np.frombuffer(buf, np.uint8).reshape(H, W, 3)
                self.count += 1
                self.cond.notify_all()

    def frames(self, n=6, skip=4):
        """Return n fresh frames (after discarding `skip`) as float array."""
        out = []
        with self.cond:
            start = self.count
            while len(out) < n:
                self.cond.wait(2)
                if self.count - start > skip and (not out or self.frame is not out[-1]):
                    out.append(self.frame)
        return np.stack(out).astype(np.float32)

    def measure(self, n=6, skip=4):
        """Mean (R,G,B) over the ROI, 0..255, plus saturated-pixel fraction."""
        f = self.frames(n, skip).mean(axis=0)
        px = f[self.roi] if self.roi is not None else f.reshape(-1, 3)
        return tuple(round(float(v), 1) for v in px.mean(axis=0)), round(float((px.max(axis=1) > 250).mean()), 3)

    def close(self):
        self.alive = False
        self.proc.terminate()
        v4l2(auto_exposure=3)
        v4l2(white_balance_automatic=1)
        v4l2(exposure_dynamic_framerate=1)


class Link:
    def __init__(self, hello=True):
        for attempt in range(8):
            self.sock = socket.socket(socket.AF_BLUETOOTH, socket.SOCK_STREAM, socket.BTPROTO_RFCOMM)
            self.sock.settimeout(15)
            try:
                self.sock.connect((ADDR, CHANNEL))
                break
            except OSError as e:
                self.sock.close()
                print(f"   connect attempt {attempt} failed: {e}")
                time.sleep(0.5 * (attempt + 1))
        else:
            raise OSError("could not connect")
        self.buf = b""
        self.records = []
        self.t0 = time.monotonic()
        if hello:
            self.raw(b"01234567")
            time.sleep(0.3)

    def raw(self, data):
        self.sock.send(data)
        self.records.append({"t": round(time.monotonic() - self.t0, 3), "dir": "TX", "hex": data.hex()})

    def frame(self, typ, cmd, body):
        body = bytes.fromhex(body) if isinstance(body, str) else bytes(body)
        self.raw(MAGIC + bytes([typ, cmd]) + (8 + len(body)).to_bytes(2, "little") + body)

    def light(self, g=0, b=0, r=0, speed=0, effect=0x50, w=0, y=0, auto=0):
        self.frame(0x53, 0x83, bytes([g, b, r, speed, effect, w, y, auto]))

    def recv_frame(self, timeout=1.5):
        self.sock.settimeout(timeout)
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if len(self.buf) >= 8 and self.buf[:4] == MAGIC:
                ln = int.from_bytes(self.buf[6:8], "little")
                if len(self.buf) >= ln >= 8:
                    f, self.buf = self.buf[:ln], self.buf[ln:]
                    self.records.append({"t": round(time.monotonic() - self.t0, 3), "dir": "RX", "hex": f.hex()})
                    return f
            try:
                self.buf += self.sock.recv(1024)
            except socket.timeout:
                break
        return None

    def query(self, cmd, body="0000000000000000"):
        self.frame(0x51, cmd, body)
        return self.recv_frame()

    def state(self):
        f = self.query(0x82)
        return f[8:].hex() if f else None

    def close(self, note=""):
        self.sock.close()
        with open(LOG, "a") as fh:
            fh.write(json.dumps({"when": time.strftime("%Y-%m-%dT%H:%M:%S"), "note": note, "records": self.records}) + "\n")
