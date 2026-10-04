"""Raw SDP ServiceSearchAttribute query over L2CAP PSM 1 (read-only discovery).

Usage: python3 sdp_dump.py AA:BB:CC:DD:EE:FF
"""
import socket
import struct
import sys

ADDR = sys.argv[1]
PSM_SDP = 0x0001


def de_uuid16(u):
    return bytes([0x19]) + struct.pack(">H", u)


def de_seq(payload):
    return bytes([0x35, len(payload)]) + payload


def request(tid, uuid16, cont=b"\x00"):
    pattern = de_seq(de_uuid16(uuid16))
    # attribute range 0x0000-0xffff
    attrs = de_seq(bytes([0x0A]) + struct.pack(">I", 0x0000FFFF))
    params = pattern + struct.pack(">H", 0xFFFF) + attrs + cont
    return struct.pack(">BHH", 0x06, tid, len(params)) + params


def parse(data, pos=0, depth=0, out=None):
    """Parse data elements, returning (value, newpos)."""
    hdr = data[pos]
    dtype, sidx = hdr >> 3, hdr & 7
    pos += 1
    if dtype == 0:
        return None, pos
    if sidx < 5:
        size = 1 << sidx
    elif sidx == 5:
        size = data[pos]
        pos += 1
    elif sidx == 6:
        size = struct.unpack(">H", data[pos:pos + 2])[0]
        pos += 2
    else:
        size = struct.unpack(">I", data[pos:pos + 4])[0]
        pos += 4
    raw = data[pos:pos + size]
    end = pos + size
    if dtype in (1, 2):
        return ("int" if dtype == 2 else "uint", int.from_bytes(raw, "big")), end
    if dtype == 3:
        return ("uuid", raw.hex()), end
    if dtype == 4:
        return ("str", raw.decode("utf-8", "replace")), end
    if dtype == 5:
        return ("bool", bool(raw[0])), end
    if dtype in (6, 7):
        items = []
        p = pos
        while p < end:
            v, p = parse(data, p)
            items.append(v)
        return ("seq" if dtype == 6 else "alt", items), end
    if dtype == 8:
        return ("url", raw.decode("utf-8", "replace")), end
    return ("raw", raw.hex()), end


def show(v, indent=0):
    pad = "  " * indent
    kind, val = v
    if kind in ("seq", "alt"):
        print(f"{pad}{kind}:")
        for it in val:
            show(it, indent + 1)
    elif kind == "uint":
        print(f"{pad}uint 0x{val:x} ({val})")
    else:
        print(f"{pad}{kind} {val}")


def query(uuid16):
    s = socket.socket(socket.AF_BLUETOOTH, socket.SOCK_SEQPACKET, socket.BTPROTO_L2CAP)
    s.settimeout(10)
    s.connect((ADDR, PSM_SDP))
    body = b""
    cont = b"\x00"
    tid = 1
    while True:
        s.send(request(tid, uuid16, cont))
        rsp = s.recv(4096)
        pdu, rtid, plen = struct.unpack(">BHH", rsp[:5])
        if pdu != 0x07:
            print(f"unexpected pdu 0x{pdu:02x}: {rsp.hex()}")
            break
        count = struct.unpack(">H", rsp[5:7])[0]
        body += rsp[7:7 + count]
        clen = rsp[7 + count]
        if clen == 0:
            break
        cont = rsp[7 + count:7 + count + 1 + clen]
        tid += 1
    s.close()
    return body


if __name__ == "__main__":
    for label, u in (("L2CAP (all records)", 0x0100),):
        body = query(u)
        print(f"== {label}: {len(body)} bytes ==")
        print(body.hex())
        v, _ = parse(body)
        # top-level: seq of records, each record seq of (attrid, value) pairs
        for n, rec in enumerate(v[1]):
            print(f"--- record {n} ---")
            items = rec[1]
            for i in range(0, len(items), 2):
                aid = items[i][1]
                print(f" attr 0x{aid:04x}:")
                show(items[i + 1], 2)
