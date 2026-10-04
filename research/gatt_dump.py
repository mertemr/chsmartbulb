"""Read-only GATT discovery over a raw LE ATT socket (fixed channel 4).

Does not go through bluetoothd, so the existing BR/EDR link is untouched.
Usage: python3 gatt_dump.py AA:BB:CC:DD:EE:FF [public|random]
"""
import socket
import struct
import sys

ADDR = sys.argv[1]
ATYPE = socket.BDADDR_LE_RANDOM if len(sys.argv) > 2 and sys.argv[2] == "random" else socket.BDADDR_LE_PUBLIC
ATT_CID = 4

s = socket.socket(socket.AF_BLUETOOTH, socket.SOCK_SEQPACKET, socket.BTPROTO_L2CAP)
s.settimeout(20)
s.bind((socket.BDADDR_ANY, 0, ATT_CID, socket.BDADDR_LE_PUBLIC))
s.connect((ADDR, 0, ATT_CID, ATYPE))
print("LE ATT connected")
s.settimeout(5)


def req(pdu):
    s.send(pdu)
    while True:
        r = s.recv(512)
        # ignore server-initiated requests/notifications
        if r[0] in (0x1B, 0x1D):
            continue
        if r[0] == 0x02 and pdu[0] != 0x02:
            continue
        return r


def uuid_str(b):
    if len(b) == 2:
        return f"0x{struct.unpack('<H', b)[0]:04x}"
    return b[::-1].hex()


r = req(struct.pack("<BH", 0x02, 185))
print("MTU rsp:", r.hex())

# primary services
services = []
start = 1
while start <= 0xFFFF:
    r = req(struct.pack("<BHHH", 0x10, start, 0xFFFF, 0x2800))
    if r[0] != 0x11:
        break
    ln = r[1]
    for i in range(2, len(r), ln):
        h, e = struct.unpack("<HH", r[i:i + 4])
        services.append((h, e, uuid_str(r[i + 4:i + ln])))
    start = services[-1][1] + 1
    if services[-1][1] == 0xFFFF:
        break
print("\nPrimary services:")
for h, e, u in services:
    print(f"  0x{h:04x}-0x{e:04x}  {u}")

# characteristics
chars = []
start = 1
while start <= 0xFFFF:
    r = req(struct.pack("<BHHH", 0x08, start, 0xFFFF, 0x2803))
    if r[0] != 0x09:
        break
    ln = r[1]
    for i in range(2, len(r), ln):
        decl = struct.unpack("<H", r[i:i + 2])[0]
        props = r[i + 2]
        vh = struct.unpack("<H", r[i + 3:i + 5])[0]
        chars.append((decl, props, vh, uuid_str(r[i + 5:i + ln])))
    start = chars[-1][0] + 1
PROPS = ["broadcast", "read", "write-no-rsp", "write", "notify", "indicate", "signed-write", "ext"]
print("\nCharacteristics:")
for decl, props, vh, u in chars:
    names = [n for b, n in enumerate(PROPS) if props & (1 << b)]
    print(f"  decl 0x{decl:04x} value 0x{vh:04x} uuid {u} props 0x{props:02x} {names}")

# all attributes (find information)
print("\nAll attributes (handle -> type):")
attrs = []
start = 1
while start <= 0xFFFF:
    r = req(struct.pack("<BHH", 0x04, start, 0xFFFF))
    if r[0] != 0x05:
        break
    fmt = r[1]
    step = 4 if fmt == 1 else 18
    for i in range(2, len(r), step):
        h = struct.unpack("<H", r[i:i + 2])[0]
        attrs.append((h, uuid_str(r[i + 2:i + step])))
    start = attrs[-1][0] + 1
for h, u in attrs:
    r = req(struct.pack("<BH", 0x0A, h))
    if r[0] == 0x0B:
        val = r[1:]
        txt = val.decode("ascii", "replace") if all(32 <= c < 127 for c in val) and val else ""
        print(f"  0x{h:04x} {u:>36}  read={val.hex()} {txt!r}")
    else:
        print(f"  0x{h:04x} {u:>36}  read-error=0x{r[4]:02x}" if len(r) >= 5 else f"  0x{h:04x} {u} rsp={r.hex()}")
s.close()
