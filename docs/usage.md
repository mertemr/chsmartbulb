# Usage

## Command line

Pass the bulb's address with `--address` or set it once:

```bash
export CHSMARTBULB_ADDRESS=AA:BB:CC:DD:EE:FF
```

| Command | Does |
|---|---|
| `info` | Name, version, model |
| `state` | On/off and colour mix |
| `on` / `off` `[--fade]` | Switch the light |
| `color COLOR [-b PCT] [--fade]` | Name (`red`, `white`, ...) or hex (`ff0064`, or `rrggbbww` with the white channel) |
| `rgb R G B [-w WHITE] [-b PCT] [--fade]` | Colour by channel values |
| `white [PCT]` | White LEDs only |
| `brightness PCT` | Dim the current colour |
| `native NAME [-c COLOR] [-s 0..15]` | Effect built into the bulb (`breathing`, `rainbow`, `flash`, `music`, ...) |
| `effect NAME [-c COLOR] [-p PERIOD] [-d SECONDS]` | Effect generated on the computer (`breathe`, `hue`, `pulse`, `strobe`, `police`) |
| `timers` | List schedule entries stored in the bulb |
| `timer INDEX on\|off` | Enable or disable a stored entry |
| `raw HEX` | Send one raw frame; prints the answer to a query |

Each invocation is a separate connection and the bulb does not report brightness, so the CLI is
stateless: `brightness` re-sends the colour mix read from the bulb at the requested level, and
`on` after `off` comes back white because the previous colour is gone.

## Library

```python
import asyncio
from chsmartbulb import ChSmartBulb, Color

async def main():
    async with ChSmartBulb.rfcomm("AA:BB:CC:DD:EE:FF") as bulb:
        await bulb.set_rgb(255, 0, 100)
        await bulb.set_brightness(0.4)
        await bulb.set_color(Color.from_hsv(200), fade=True)   # one second soft transition
        await bulb.turn_off()
        await bulb.turn_on()                                    # back to the last colour
        print(await bulb.get_info())

asyncio.run(main())
```

A blocking wrapper for scripts and the REPL:

```python
from chsmartbulb import BlockingLight, ChSmartBulb

bulb = BlockingLight(ChSmartBulb.rfcomm("AA:BB:CC:DD:EE:FF"))
bulb.connect()
bulb.set_rgb(255, 0, 100)
bulb.set_brightness(0.8)
bulb.disconnect()
```

`ChSmartBulb.ble(address)` uses BLE instead (needs the `ble` extra). On Linux read
[the BlueZ note](device.md#ble-on-linux--bluez) first.

### Things to know

- Brightness is kept by the library and applied to every colour it sends. The bulb cannot report
  it, so it is assumed to be 1.0 after connecting.
- `Color` has a separate white channel: `Color(w=255)` lights the white LEDs.
- If the link drops, the next command reconnects once. Pass `auto_reconnect=False` to turn that off.
- Errors derive from `SmartBulbError`: `ConnectionFailed`, `NotConnected`, `TransportError`,
  `RequestTimeout`, `ProtocolError`.

### Built-in effects

```python
from chsmartbulb import NativeEffect

await bulb.set_native_effect(NativeEffect.BREATHING, Color(g=255), speed=4)   # 0 fastest, 15 slowest
await bulb.set_native_effect(NativeEffect.MUSIC)   # reacts to audio played through the bulb
```

### Your own effects

An effect is a function from elapsed seconds to a `Color`. The library samples it at a fixed rate
and streams the result.

```python
from chsmartbulb import Color, effects

await effects.play(bulb, effects.hue_cycle(period=10), duration=30)

player = effects.EffectPlayer(bulb, fps=20)
await player.start(effects.breathe(Color(r=255, g=60), period=4))
...
await player.stop()

# custom sequence: (colour, hold seconds, fade-in seconds)
alarm = effects.sequence([(Color(r=255), 0.3), (Color(), 0.3), (Color(b=255), 1.0, 0.5)])

# anything else
def flicker(t: float) -> Color:
    return Color(r=255, g=int(80 + 60 * abs((t * 3) % 2 - 1)))
```

Building blocks: `solid`, `breathe`, `hue_cycle`, `pulse`, `strobe`, `fade`, `sequence`, `dimmed`.

The bulb follows about 25 colour changes per second over RFCOMM and about 13 over BLE; the default
is 20 fps and slower links simply send fewer frames. `set_brightness()` also dims a running effect.

## Layout

```
chsmartbulb.Light          device-independent interface (colour, brightness, on/off)
  ChSmartBulb              this bulb: state tracking, queries, reconnect
    protocol               frame encoding and parsing, no I/O
    transport.Transport    byte pipe
      RfcommTransport      Bluetooth Classic, standard library only
      BleTransport         BLE GATT via bleak
chsmartbulb.effects        effect engine, depends on Light only
```
