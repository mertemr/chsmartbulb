"""Send protocol frames to the 0x8888 / 0x8877 characteristics via BlueZ D-Bus and watch for notifications + light."""
import time
import dbus, dbus.mainloop.glib
from gi.repository import GLib
import rig

dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
bus = dbus.SystemBus()
DEV = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF"
C8888 = DEV + "/service000b/char000c"
C8877 = DEV + "/service000f/char0010"
ctx = GLib.MainContext.default()
t0 = time.monotonic()

def on_change(iface, changed, invalidated, path=None):
    if "Value" in changed:
        print(f"   [{time.monotonic()-t0:6.2f}] NOTIFY/CHG {path[-8:]}: {bytes(changed['Value']).hex()}")

bus.add_signal_receiver(on_change, dbus_interface="org.freedesktop.DBus.Properties", signal_name="PropertiesChanged", path_keyword="path")

def pump(t):
    end = time.monotonic() + t
    while time.monotonic() < end:
        ctx.iteration(False); time.sleep(0.01)

def char(p):
    return dbus.Interface(bus.get_object("org.bluez", p), "org.bluez.GattCharacteristic1")

def props(p):
    o = dbus.Interface(bus.get_object("org.bluez", p), "org.freedesktop.DBus.Properties")
    return o.GetAll("org.bluez.GattCharacteristic1")

for p in (C8888, C8877):
    pr = props(p); print(p[-8:], "uuid", pr["UUID"], "flags", [str(f) for f in pr["Flags"]])

cam = rig.Camera()
print("cam start:", cam.measure())
c1, c2 = char(C8888), char(C8877)
try:
    c1.StartNotify(); print("StartNotify ok")
except Exception as e:
    print("StartNotify failed:", e)
pump(0.5)
Q = bytes.fromhex("01fe0000510210000000008000000080")
Q82 = bytes.fromhex("01fe0000518210000000000000000000")
RED = bytes.fromhex("01fe0000538310000000ff0050000000")
BLUE = bytes.fromhex("01fe00005383100000ff000050000000")
WHITE = bytes.fromhex("01fe0000538310000000000050ff0000")

def w(c, data, tag, typ="request"):
    try:
        c.WriteValue(dbus.Array(data, signature="y"), {"type": typ})
        print(f"[{time.monotonic()-t0:6.2f}] {tag}: wrote {data.hex()} ({typ})")
    except Exception as e:
        print(f"{tag}: write failed: {e}")
    pump(1.2)

w(c1, b"01234567", "hello->8888")
w(c1, Q, "q02->8888")
w(c1, Q82, "q82->8888")
w(c1, RED, "red->8888"); print("   cam:", cam.measure())
w(c1, BLUE, "blue->8888", "command"); print("   cam:", cam.measure())
w(c2, Q, "q02->8877")
w(c2, RED, "red->8877"); print("   cam:", cam.measure())
w(c2, WHITE, "white->8877", "command"); print("   cam:", cam.measure())
for name, c in (("8888", c1), ("8877", c2)):
    try:
        v = bytes(c.ReadValue({})); print(f"read {name}: {v[:24].hex()}… len={len(v)}")
    except Exception as e:
        print("read failed", e)
try:
    c1.StopNotify()
except Exception:
    pass
cam.close()
