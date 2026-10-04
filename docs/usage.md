# Usage

## Command line

Pass the bulb's address with `--address` or set it once:

```bash
export CHSMARTBULB_ADDRESS=AA:BB:CC:DD:EE:FF
```

| Command | Does |
|---|---|
| `status` | Connection, light state and running effect |
| `info` | Name, version, model |
| `on` / `off` `[--fade]` | Switch the light |
| `color COLOR [-b PCT] [--fade]` | Name (`red`, `white`, ...) or hex (`ff0064`, or `rrggbbww` with the white channel) |
| `rgb R G B [-w WHITE] [-b PCT] [--fade]` | Colour by channel values |
| `white [PCT]` | White LEDs only |
| `brightness PCT` | Dim whatever is showing, effects included |
| `effect NAME [-c COLOR] [-p PERIOD] [-s NAME=VALUE] [-d SECONDS]` | Effect generated on the computer |
| `effects` | List those effects and their parameters |
| `native NAME [-c COLOR] [-s 0..15]` | Effect built into the bulb (`breathing`, `rainbow`, `flash`, `music`, ...) |
| `stop` | Stop the effect and return to the plain colour |
| `timers` | List schedule entries stored in the bulb |
| `timer INDEX on\|off` | Enable or disable a stored entry |
| `raw HEX` | Send one raw frame; prints the answer to a query |
| `daemon` | Run the background service |

Without the service every invocation is a separate connection. The bulb does not report
brightness and forgets nothing but the colour mix, so in that mode `on` after `off` comes back
white and an effect runs only as long as the command does.

## Background service

```bash
chsmartbulb daemon
```

The service keeps the connection open, runs effects in the background and remembers the light
state (colour, brightness, on/off, effect). While it runs, the other commands talk to it instead
of the bulb, which makes them fast and lets `effect` return immediately.

- If the bulb loses power or goes out of range, the service keeps retrying and puts the
  remembered state back when the bulb returns. Requests made in the meantime are applied then.
- The state is stored in `$XDG_STATE_HOME/chsmartbulb/state.json` and restored on start
  (`--no-state` turns that off).
- It listens on `$XDG_RUNTIME_DIR/chsmartbulb.sock` (`--socket` changes it). `--direct` makes a
  command bypass the service, which only works when the service is not holding the connection.

To start it with your session, copy [`contrib/chsmartbulb.service`](../contrib/chsmartbulb.service)
to `~/.config/systemd/user/`, fill in the address and the path, then:

```bash
systemctl --user enable --now chsmartbulb
```

### Socket protocol

One JSON object per line in each direction. Replies carry `"ok"` and, on failure, `"error"`.

```json
{"cmd": "color", "color": "#ff0064", "brightness": 0.5, "fade": true}
{"cmd": "effect", "name": "breathe", "params": {"color": "#00ff00", "period": 3}, "duration": 60}
{"cmd": "status"}
```

Commands: `status`, `on`, `off`, `color`, `brightness`, `effect`, `native`, `stop`, `effects`,
`info`, `timers`, `timer`, `raw`.

## Effects

`chsmartbulb effects` lists what is available:

| Name | Does | Parameters |
|---|---|---|
| `breathe` | Swell and fade | `color`, `period`, `floor` |
| `hue` | Walk around the colour wheel | `period`, `saturation` |
| `pulse` | Flash, then decay | `color`, `period`, `decay` |
| `strobe` | Hard blinking | `color`, `hz`, `duty` |
| `candle` | Uneven flicker | `color`, `depth` |
| `palette` | Drift through a list of colours | `colors`, `hold`, `fade_in` |
| `police` | Alternate red and blue | `period` |
| `music` | Flash on the beat | `color`, `decay`, `delay` |
| `spectrum` | Bass, mids and treble as red, green and blue | `release`, `delay` |

```bash
chsmartbulb effect breathe -c 00ff00 -p 3
chsmartbulb effect palette -s colors=red,ff8000,blue -s hold=5
chsmartbulb effect music -b 60
```

### Sound-reactive effects

`music` and `spectrum` analyse the audio on the computer, taken from the monitor of the default
output. It does not matter where the sound plays: laptop speakers, headphones, another Bluetooth
device or the bulb itself. They need the `audio` extra (numpy) and the `parec` tool that comes
with PulseAudio and PipeWire.

`--audio-device SOURCE` picks a different source, for example the monitor of one particular
output (`pactl list short sources` shows the names).

The capture hears the sound before a Bluetooth speaker or headphones play it, so the light runs
ahead of what you hear. `delay` holds the light back by that many seconds (up to 2):

```bash
chsmartbulb effect music -s delay=0.2
```

Bluetooth outputs usually need 0.15 to 0.3; wired outputs need none.

This is separate from the bulb's own `native music` mode, which reacts to sound played through
the bulb's speaker.

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

Building blocks: `solid`, `breathe`, `hue_cycle`, `pulse`, `strobe`, `candle`, `palette`, `fade`,
`sequence`, `dimmed`. `catalog.create(name, params)` builds one from plain data.

Sound-reactive effects read from a `MusicSource`:

```python
from chsmartbulb import effects, music

source = music.MusicSource()
await source.start()
await effects.play(bulb, music.music_pulse(source), duration=60)
await source.stop()
```

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
chsmartbulb.music          audio capture and analysis for the sound-reactive effects
chsmartbulb.catalog        effects by name with plain parameters
chsmartbulb.service        background service and its socket protocol
```
