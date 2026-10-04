"""Physical check of the chsmartbulb library using the camera rig."""
import asyncio, sys, time
import numpy as np
sys.path.insert(0, str(__import__("pathlib").Path(__file__).resolve().parent.parent / "src"))
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
    async with ChSmartBulb.rfcomm(ADDR) as bulb:
        print("connected; on =", bulb.is_on, "colour =", bulb.color.to_hex())
        await bulb.turn_off(); dark = await measure()
        def d(v): return tuple(round(float(x), 1) for x in (v - dark))
        await bulb.set_rgb(255, 0, 0); print("set_rgb(255,0,0)        ->", d(await measure()))
        await bulb.set_brightness(0.5); print("set_brightness(0.5)     ->", d(await measure()))
        await bulb.set_brightness(0.1); print("set_brightness(0.1)     ->", d(await measure()))
        await bulb.set_color(Color(b=255)); print("set_color(blue) @0.1    ->", d(await measure()))
        await bulb.set_brightness(1.0); print("set_brightness(1.0)     ->", d(await measure()))
        await bulb.turn_off(); print("turn_off()              ->", d(await measure()), "state.is_on =", (await bulb.get_light_state()).is_on)
        await bulb.turn_on(); print("turn_on()               ->", d(await measure()))
        await bulb.set_white(); print("set_white()             ->", d(await measure()))
        # fade: sample shortly after the command
        await bulb.set_color(Color(r=255)); await asyncio.sleep(0.8)
        await bulb.set_color(Color(g=255), fade=True)
        print("fade red->green at 0.4 s ->", d(await measure(0.3)), " at 1.6 s ->", d(await measure(1.0)))
        # host effect: hue cycle, 3 s period, sample which channel dominates over time
        player = effects.EffectPlayer(bulb, fps=20)
        await player.start(effects.hue_cycle(period=3.0))
        seen = []
        t0 = time.monotonic()
        while time.monotonic() - t0 < 3.0:
            v = (await measure(0.12)) - dark
            seen.append("RGB"[int(np.argmax(v))])
        await player.stop()
        print("hue_cycle dominant channel over 3 s:", "".join(seen))
        await player.start(effects.strobe(Color(r=255), hz=2.0))
        lv = []
        t0 = time.monotonic()
        while time.monotonic() - t0 < 2.0:
            f = await loop.run_in_executor(None, lambda: cam.frames(1, 0)[0])
            lv.append(float(f[cam.roi].mean(axis=0)[0] - dark[0]) > 30)
        await player.stop()
        print("strobe 2 Hz: on/off flips seen in 2 s:", int(np.abs(np.diff(np.array(lv, int))).sum()))
        # reconnect: kill the socket underneath the library
        bulb._transport._sock.close(); bulb._transport._sock = None
        await bulb.set_color(Color(r=255, b=255))
        print("after forced socket close, set magenta ->", d(await measure()), "connected =", bulb.is_connected)
        await bulb.set_white()
        print("restored:", (await bulb.get_light_state()).color.to_hex())
    cam.close()

asyncio.run(main())
