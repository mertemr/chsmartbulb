"""Experiment probe for the SmartBulb SPP protocol.

Usage: python3 probe.py [--no-hello] [--log FILE] STEP [STEP...]
  STEP = hex string  -> send those bytes
         wN          -> wait N seconds (float) while printing anything received
Every TX/RX is printed with a relative timestamp and appended to the log.
"""
import json
import socket
import sys
import threading
import time

ADDR = "AA:BB:CC:DD:EE:FF"
CHANNEL = 2
MAGIC = bytes.fromhex("01fe0000")

args = sys.argv[1:]
hello = True
logfile = None
if "--no-hello" in args:
    args.remove("--no-hello")
    hello = False
if "--log" in args:
    i = args.index("--log")
    logfile = args[i + 1]
    del args[i:i + 2]

t0 = time.monotonic()
lock = threading.Lock()
records = []


def note(direction, data):
    ts = time.monotonic() - t0
    with lock:
        print(f"{ts:7.3f} {direction} {data.hex()}", flush=True)
        records.append({"t": round(ts, 3), "dir": direction, "hex": data.hex()})


def reader(sock, stop):
    buf = b""
    sock.settimeout(0.2)
    while not stop.is_set():
        try:
            chunk = sock.recv(1024)
        except socket.timeout:
            if buf:
                note("RX?", buf)  # leftover that never formed a frame
                buf = b""
            continue
        except OSError as e:
            print(f"reader closed: {e}", flush=True)
            return
        if not chunk:
            print("reader: peer closed", flush=True)
            return
        buf += chunk
        # split into frames using the little-endian length at offset 6
        while len(buf) >= 8 and buf[:4] == MAGIC:
            ln = int.from_bytes(buf[6:8], "little")
            if ln < 16 or len(buf) < ln:
                break
            note("RX ", buf[:ln])
            buf = buf[ln:]


sock = socket.socket(socket.AF_BLUETOOTH, socket.SOCK_STREAM, socket.BTPROTO_RFCOMM)
sock.settimeout(15)
sock.connect((ADDR, CHANNEL))
print(f"connected rfcomm ch{CHANNEL} in {time.monotonic() - t0:.2f}s", flush=True)
stop = threading.Event()
th = threading.Thread(target=reader, args=(sock, stop), daemon=True)
th.start()

try:
    if hello:
        sock.send(b"01234567")
        note("TX ", b"01234567")
        time.sleep(0.3)
    for step in args:
        if step.startswith("w"):
            time.sleep(float(step[1:]))
        else:
            data = bytes.fromhex(step)
            sock.send(data)
            note("TX ", data)
            time.sleep(0.15)
finally:
    time.sleep(0.5)
    stop.set()
    th.join(1)
    sock.close()
    if logfile:
        with open(logfile, "a") as f:
            f.write(json.dumps({"when": time.strftime("%Y-%m-%dT%H:%M:%S"), "steps": args, "records": records}) + "\n")
