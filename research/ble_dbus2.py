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


def lf(g=0,b=0,r=0,sp=0,ef=0x50,wv=0,y=0,fd=0):
    return bytes.fromhex("01fe000053831000") + bytes([g,b,r,sp,ef,wv,y,fd])
w(c2, lf(wv=255), "white->8877 request"); print("   cam:", cam.measure())
w(c2, lf(b=255), "blue->8877 command", "command"); print("   cam:", cam.measure())
w(c2, lf(g=255), "green->8877 command #2", "command"); print("   cam:", cam.measure())
w(c2, lf(r=255), "red->8877 request"); print("   cam:", cam.measure())
w(c2, Q82, "q82->8877")
w(c2, bytes.fromhex("01fe0000518010000000000000000000"), "q80->8877")
w(c2, bytes.fromhex("01fe0000510010000000008000000080"), "q00->8877")
w(c2, bytes.fromhex("01fe0000513010000000008000000080"), "q30->8877")
# no-StartNotify needed? rate test: 20 writes with response
t=time.monotonic()
for i in range(20):
    c2.WriteValue(dbus.Array(lf(r=255) if i%2 else lf(b=255), signature="y"), {"type":"request"})
print(f"20 request-writes took {time.monotonic()-t:.2f}s")
pump(0.5); print("   cam:", cam.measure())
w(c2, lf(wv=255), "white->8877 request (restore)"); print("   cam:", cam.measure())
try:
    c1.StopNotify()
except Exception:
    pass
cam.close()
