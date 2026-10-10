# chsmartbulb

<img src="docs/images/bulb.jpg" alt="The bulb, as its sellers picture it with the vendor app" width="220" align="right">

Control the CHSmartBulb / BL08A Bluetooth speaker bulb from a computer, without the vendor app.

The app (CHSmartBulb) no longer runs on current phones, which leaves these bulbs stuck on whatever
colour they had last. This project documents the bulb's protocol and provides a background
service, an app, a web interface, a command line tool and a Python library for it.

## What works

- RGB colour, the separate white LEDs, brightness, on/off, soft fades
- The 13 effects built into the bulb, including its sound-reactive mode
- Your own effects, generated on the computer and streamed to the bulb, including ones that
  follow whatever audio the computer is playing
- A background service (`chsmartbulbd`, one Rust program) that keeps the connection, runs effects
  and restores the light when the bulb comes back after losing power
- A web interface served by that service, for phones and other computers on the network
- Control from other machines on the network, with agents so the light can follow the music or
  the screen of a computer that has no Bluetooth
- Reading the name, model, colour and stored timers; enabling and disabling timers
- Bluetooth Classic (RFCOMM) and BLE transports

Colour temperature is not supported by the hardware.

## Web interface

`chsmartbulbd --web 8378` serves a page for phones and other computers on the network; see
[the usage notes](docs/usage.md#web-interface).

<p>
  <img src="docs/images/web-desktop.png" alt="The web interface on a wide screen: brightness, connections and the colour wheel" width="100%">
</p>
<p>
  <img src="docs/images/web-colour.png" alt="Choosing a colour on a phone" width="32%">
  <img src="docs/images/web-custom.png" alt="Editing the steps of a custom effect" width="32%">
  <img src="docs/images/web-sound.png" alt="The effects that follow the sound" width="32%">
</p>

## App

`app/` is a desktop and mobile app (Android, Linux, Windows) with the same interface. It talks to
the bulb itself over BLE or Bluetooth Classic, so a phone can drive the bulb with nothing else
running, and its sound effects can follow what the phone plays while the sound goes to another
speaker. Builds come from the [`app` workflow](.github/workflows/app.yml); see
[docs/app.md](docs/app.md).

## Install

The service and the app come ready-made with every
[release](https://github.com/mertemr/chsmartbulb/releases): `chsmartbulbd` for Linux and Windows,
the app for Android, Linux and Windows.

The command line and the Python library need Linux with BlueZ, a paired bulb, and Python 3.10 or
newer with Bluetooth socket support:

```bash
git clone https://github.com/mertemr/chsmartbulb.git
cd chsmartbulb
uv sync
cargo build --release -p chsmartbulb-daemon   # the service, target/release/chsmartbulbd
```

Python builds downloaded by `uv` are compiled without Bluetooth sockets, so the project is set up
to use the system interpreter. Add `--extra audio` or `--extra screen` for the agents that feed a
service the sound or the screen of another computer, and `--extra ble` for the BLE transport.

The effects are written once, in Rust. The service and the app carry them; the Python package
plays them too once `uv pip install ./crates/python` (which needs a Rust toolchain) has put
`chsmartbulb-native` next to it.

## Quick start

```bash
mkdir -p ~/.config/chsmartbulb
echo CHSMARTBULB_ADDRESS=AA:BB:CC:DD:EE:FF > ~/.config/chsmartbulb/config
uv run chsmartbulb color red
uv run chsmartbulb rgb 0 80 255 --brightness 40 --fade
chsmartbulbd &                                # the background service
uv run chsmartbulb effect hue --period 10
```

With `chsmartbulbd` running, the commands go through it: they return at once, effects keep
playing, and the light state is remembered.

```python
import asyncio
from chsmartbulb import ChSmartBulb, Color, effects

async def main():
    async with ChSmartBulb.rfcomm("AA:BB:CC:DD:EE:FF") as bulb:
        await bulb.set_rgb(255, 0, 100)
        await bulb.set_brightness(0.4)
        await effects.play(bulb, lambda t: Color.from_hsv(36 * t), duration=20)

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
- [docs/app.md](docs/app.md): the app and the Rust core
- [research/](research): the scripts and raw results behind the above

## Tests

```bash
cargo test --workspace
uv run pytest
```

The tests need no hardware; they run against a simulated bulb that behaves like the real one.

## Credits

Earlier work on these bulbs that this project started from:
[pfalcon/Chsmartbulb-led-bulb-speaker](https://github.com/pfalcon/Chsmartbulb-led-bulb-speaker),
[samsam2310/Bluetooth-Chsmartbulb-Python-API](https://github.com/samsam2310/Bluetooth-Chsmartbulb-Python-API),
[rafaelbiasi/ChSmartBulbAPI](https://github.com/rafaelbiasi/ChSmartBulbAPI) and
[roelderickx/bluetooth-smartbulb](https://github.com/roelderickx/bluetooth-smartbulb).

The picture of the bulb is the sellers' product picture, as kept in the first and third of those
repositories. The screenshots are of the web interface driving a simulated bulb.

## License

MIT
