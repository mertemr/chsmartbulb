"""Physical check of ChSmartBulb over the bleak BLE transport (needs the Classic link to be down)."""
import asyncio
import pathlib
import sys
import time

import numpy as np

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "src"))
import rig
from chsmartbulb import ChSmartBulb, Color, effects

ADDR = "AA:BB:CC:DD:EE:FF"


async def main():
    cam = rig.Camera()
    loop = asyncio.get_running_loop()

    async def measure(settle=0.8):
        await asyncio.sleep(settle)
        (r, g, b), _ = await loop.run_in_executor(None, cam.measure)
        return np.array([r, g, b])

    t0 = time.monotonic()
    # BlueZ only: the LE link must already be up (see docs/device.md), and bleak cannot
    # find a connected device by scanning, so hand it the BlueZ object path directly.
    from bleak.backends.device import BLEDevice
    from chsmartbulb import protocol as p
    from chsmartbulb.transport import BleTransport

    device = BLEDevice(ADDR, "SmartBulb Bluetooth", {"path": "/org/bluez/hci0/dev_" + ADDR.replace(":", "_"), "props": {}})
    async with ChSmartBulb(BleTransport(device, p.BLE_WRITE_UUID, p.BLE_NOTIFY_UUID)) as bulb:
        print(f"BLE connected in {time.monotonic() - t0:.1f}s; on={bulb.is_on} colour={bulb.color.to_hex()}")
        print("info  :", await bulb.get_info())
        print("timers:", [(t.index, t.name, t.enabled, f"{t.hour:02d}:{t.minute:02d}") for t in await bulb.get_timers()])
        print("status:", (await bulb.get_status_raw()).hex(" "))
        await bulb.turn_off()
        dark = await measure()

        def d(v):
            return tuple(round(float(x), 1) for x in (v - dark))

        await bulb.set_rgb(255, 0, 0)
        print("red            ->", d(await measure()), (await bulb.get_light_state()).color.to_hex())
        await bulb.set_brightness(0.2)
        print("brightness 0.2 ->", d(await measure()))
        await bulb.set_brightness(1.0)
        await bulb.set_color(Color(b=255), fade=True)
        print("blue (fade)    ->", d(await measure(1.5)), (await bulb.get_light_state()).color.to_hex())
        t = time.monotonic()
        for i in range(30):
            await bulb.set_color(Color(r=255) if i % 2 else Color(g=255))
        print(f"30 colour writes took {time.monotonic() - t:.2f}s")
        sent = []
        original = bulb.set_color

        async def counting(color, **kw):
            sent.append(time.monotonic())
            await original(color, **kw)

        bulb.set_color = counting
        await effects.play(bulb, effects.hue_cycle(period=3.0), duration=3.0, fps=20)
        bulb.set_color = original
        print(f"hue_cycle at 20 fps requested: {len(sent)} frames sent in 3 s")
        await bulb.set_white()
        print("white          ->", d(await measure()), (await bulb.get_light_state()).color.to_hex())
    cam.close()


asyncio.run(main())
