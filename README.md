# chsmartbulb

Control the CHSmartBulb / BL08A Bluetooth speaker bulb from a computer, without the vendor app.

The app (CHSmartBulb) no longer runs on current phones, which leaves these bulbs stuck on whatever
colour they had last. This project documents the bulb's protocol and provides a Python library and
a command line tool for it.

## What works

- RGB colour, the separate white LEDs, brightness, on/off, soft fades
- The 13 effects built into the bulb, including its sound-reactive mode
- Your own effects, generated on the computer and streamed to the bulb, including ones that
  follow whatever audio the computer is playing
- A background service that keeps the connection, runs effects and restores the light when the
  bulb comes back after losing power
- A web interface served by that service, for phones and other computers on the network
- Control from other machines on the network, with agents so the light can follow the music or
  the screen of a computer that has no Bluetooth
- Reading the name, model, colour and stored timers; enabling and disabling timers
- Bluetooth Classic (RFCOMM) and BLE transports

Colour temperature is not supported by the hardware.

## Install

Linux with BlueZ, a paired bulb, and Python 3.10 or newer with Bluetooth socket support.

```bash
git clone https://github.com/mertemr/chsmartbulb.git
cd chsmartbulb
uv sync
```

Python builds downloaded by `uv` are compiled without Bluetooth sockets, so the project is set up
to use the system interpreter. Add `--extra audio` for the sound-reactive effects and `--extra ble`
for the BLE transport.

## Quick start

```bash
export CHSMARTBULB_ADDRESS=AA:BB:CC:DD:EE:FF
uv run chsmartbulb color red
uv run chsmartbulb rgb 0 80 255 --brightness 40 --fade
uv run chsmartbulb effect hue --period 10
```

With `chsmartbulb daemon` running, the same commands go through the background service: they
return at once, effects keep playing, and the light state is remembered.

```python
import asyncio
from chsmartbulb import ChSmartBulb, Color, effects

async def main():
    async with ChSmartBulb.rfcomm("AA:BB:CC:DD:EE:FF") as bulb:
        await bulb.set_rgb(255, 0, 100)
        await bulb.set_brightness(0.4)
        await effects.play(bulb, effects.breathe(Color(b=255), period=4), duration=20)

asyncio.run(main())
```

More in [docs/usage.md](docs/usage.md).

## How it was worked out

The bulb is a dual-mode device. It is controlled through a serial port service on Bluetooth
Classic, and the same frames also work over two BLE characteristics that earlier write-ups had
dismissed as dummies. Every claim in the protocol document was checked on a real bulb, with the
light measured by a webcam, and is marked as measured, read back, or only seen in someone else's
capture.

- [docs/protocol.md](docs/protocol.md): frame format, commands, effects, open questions
- [docs/device.md](docs/device.md): services, connection behaviour, BLE on Linux
- [docs/research.md](docs/research.md): the experiments and the guesses that turned out wrong
- [research/](research): the scripts and raw results behind the above

## Tests

```bash
uv run pytest
```

The tests need no hardware; they run against a fake transport that behaves like the real bulb.

## Credits

Earlier work on these bulbs that this project started from:
[pfalcon/Chsmartbulb-led-bulb-speaker](https://github.com/pfalcon/Chsmartbulb-led-bulb-speaker),
[samsam2310/Bluetooth-Chsmartbulb-Python-API](https://github.com/samsam2310/Bluetooth-Chsmartbulb-Python-API),
[rafaelbiasi/ChSmartBulbAPI](https://github.com/rafaelbiasi/ChSmartBulbAPI) and
[roelderickx/bluetooth-smartbulb](https://github.com/roelderickx/bluetooth-smartbulb).

## License

MIT
