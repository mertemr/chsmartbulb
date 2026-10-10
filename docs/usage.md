# Usage

## Command line

Pass the bulb's address with `--address`, or write it down once in the settings file:
`~/.config/chsmartbulb/config` on Linux, `%APPDATA%\chsmartbulb\config` on Windows.

```bash
# the bulb in the study
CHSMARTBULB_ADDRESS=AA:BB:CC:DD:EE:FF
```

| Setting | Stands in for |
|---|---|
| `CHSMARTBULB_ADDRESS` | `--address` |
| `CHSMARTBULB_TOKEN` | `--token` |
| `CHSMARTBULB_HOST` | `--host` |
| `CHSMARTBULB_TRANSPORT` | `--transport` |
| `CHSMARTBULB_AUDIO_DEVICE` | `--audio-device` |
| `CHSMARTBULB_MONITOR` | `--monitor` |

Each line is `NAME=VALUE`; a line starting with `#` is a comment, and a value may be quoted. An
environment variable of the same name overrides the file, and the option on the command line
overrides both. A line that sets none of these is reported rather than skipped. The file may hold
the token, so keep it readable by you alone.

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
| `daemon [--listen [HOST:]PORT]` | Run the background service |
| `audio-agent` | Analyse this machine's audio and feed it to a service |
| `screen-agent` | Watch this machine's screen and feed its colour to a service |

`--host HOST[:PORT]` sends any command to a service on another machine instead.

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

`chsmartbulbd` is the same service as one Rust program, with no Python or numpy to install:

```bash
cargo build --release -p chsmartbulb-daemon      # target/release/chsmartbulbd
chsmartbulbd --address AA:BB:CC:DD:EE:FF --listen 8377 --web 8378 --token SECRET
```

It takes the daemon's options (`--address`, `--transport`, `--channel`, `--socket`, `--listen`,
`--web`, `--token`, `--no-token`, `--fps`, `--no-state`, `--mic`, `--audio-device`, `--monitor`,
`--on-lock`, `--on-sleep`), uses the same socket and state file, and the commands above talk to
it as they do to `chsmartbulb daemon`; `chsmartbulbd --help` lists them. There is no
`--audio-backend`: Linux captures with `parec`, Windows through WASAPI. It follows its own screen
on Windows and on an X11 desktop (`--monitor`). In a Wayland session nobody may read the picture
unasked, so the service has the desktop share a screen, as screen sharing in a call does: the
first `screen` effect brings up the desktop's question about which screen, the answer is
remembered, and `--choose-screen` makes it ask again. That part needs the PipeWire headers and
libclang to build (`libpipewire-0.3-dev libclang-dev` on Debian and Ubuntu, `pipewire clang` on
Arch); `--no-default-features` builds without it. `--on-lock` and `--on-sleep` work
on Linux (through logind) as well as on Windows. `--simulate` serves a simulated bulb for trying
things without one.

- If the bulb loses power or goes out of range, the service keeps retrying and puts the
  remembered state back when the bulb returns. Requests made in the meantime are applied then.
- The state is stored in `$XDG_STATE_HOME/chsmartbulb/state.json` and restored on start
  (`--no-state` turns that off).
- It listens on `$XDG_RUNTIME_DIR/chsmartbulb.sock` (`--socket` changes it). `--direct` makes a
  command bypass the service, which only works when the service is not holding the connection.

To start it with your session, copy [`contrib/chsmartbulb.service`](../contrib/chsmartbulb.service)
to `~/.config/systemd/user/`, fill in the address and the path (`chsmartbulb daemon` takes the
address from the settings file instead; `chsmartbulbd` does not read that file), then:

```bash
systemctl --user enable --now chsmartbulb
```

### Other machines

The bulb takes one Bluetooth connection, so one machine owns it and the others go through that
machine's service. Start the service with a network port and a shared secret:

```bash
export CHSMARTBULB_TOKEN=$(python3 -c "import secrets; print(secrets.token_urlsafe(16))")
chsmartbulb daemon --listen 8377
```

From anywhere else on the network, with the same token:

```bash
chsmartbulb --host laptop.local --token SECRET color red
```

The traffic is not encrypted and the token travels in clear text, so keep this to a network you
trust. Without a token the service refuses to listen on the network, unless it is started with
`--no-token`; the other machines then leave `--token` out.

### Web interface

The service can serve a page that controls the light from any browser on the network: power,
colour, the white LEDs, brightness, every effect with its parameters, the bulb's own effects, and
the device details.

```bash
chsmartbulb daemon --web 8378
```

Open `http://laptop.local:8378` and enter the token once; the browser keeps it. Every open page
follows the light live, whoever changed it, the command line included. While the bulb is out
of reach the page says why, shows what the bulb will be given when it returns, and offers to try
again at once. The same warning applies
as for `--listen`: nothing is encrypted, so keep it to a network you trust. `--web 127.0.0.1:8378`
limits it to the machine itself.

`--no-token` drops the token for `--web` and `--listen` alike: the page opens straight away and
the other machines leave `--token` out. Anyone who can reach the machine then controls the light.

What the light does is one of five modes, and choosing something in a mode switches to it: a
steady colour, a pattern that runs on time alone, an effect that follows the sound, one that
follows the screen, or an effect built into the bulb. The sound and screen modes say whose sound
or screen is being followed and how to bring an agent in; the device section lists every
connection, agents and other open pages included.

The page is a static bundle of about 30 kB that holds no logic of the service. It is built from
[`web/`](../web) (Svelte, Tailwind) and the result is kept in `src/chsmartbulb/webui`, so nothing
but Python is needed to run it. To work on it:

```bash
cd web
pnpm install
pnpm dev      # hot reload on :5173, talking to a daemon started with --web 8378
pnpm build    # writes src/chsmartbulb/webui
```

The page talks to the service over a WebSocket at `/ws`, where each text message is one object of
the [socket protocol](#socket-protocol) below, starting with `auth`. Anything that serves the
same files and answers that protocol can host it, which is what keeps a port of the service to a
microcontroller possible. The server side is in `chsmartbulb.web` and uses the standard library
only.

### When the computer locks, sleeps or shuts down (Windows)

The service can follow the computer it runs on. `--on-lock dim|off` sets what the light does
while the screen is locked, `--on-sleep dim|off` while the computer sleeps or shuts down; the
light goes back to what it showed, effect included, when the screen is unlocked or the computer
wakes. Both default to `none`. Anything that changes the light (a page, a command) ends the away
state, and the running effect, and with it the sound or screen capture, stops while it lasts.
A locked screen dims to 10 %. Sleep leaves about a second to reach the bulb; if the write does not
make it, the plan is restored after waking all the same.

```bash
chsmartbulb -t ble daemon --listen 8377 --web 0.0.0.0:8378 --on-lock dim --on-sleep off
```

To start it when you log in, a scheduled task does (put the token and the address in the settings
file rather than in the command):

```powershell
schtasks /Create /TN chsmartbulb /SC ONLOGON /TR "C:\Tools\chsmartbulb.exe -t ble daemon --listen 8377 --web 0.0.0.0:8378 --on-lock dim --on-sleep off"
```

The sound needs no agent on the computer that runs the service: it listens to its own output while
a sound effect plays.

### Audio from another machine

The sound-reactive effects need to hear the music, which may be playing on a machine that has no
Bluetooth. Run the agent there: it analyses the audio locally and sends only band levels and
beats to the service, never the audio itself.

```bash
chsmartbulb --host laptop.local --token SECRET audio-agent
```

While an agent is connected, the sound effects follow its feed; when it leaves, the service
goes back to listening on its own machine. `status` shows which one is in use.

The agent needs only the `audio` extra, no Bluetooth. Capture uses `parec` where it exists and
WASAPI loopback of the default output on Windows (through PyAudioWPatch, which the extra pulls in
there). `--audio-backend` forces one of `parec`, `wasapi` or `soundcard`; the last needs the
`soundcard` package and does not work with every Windows output device. `--audio-device NAME`
picks an output by part of its name instead of the default one.

```bash
pip install "chsmartbulb[audio] @ git+https://github.com/mertemr/chsmartbulb"
```

### The screen from another machine

The `screen` effect follows a screen, which is usually not on the machine that holds the Bluetooth
link. Run the agent where the screen is: it reduces the picture to one colour about 15 times a
second and sends only that colour.

```bash
chsmartbulb --host laptop.local --token SECRET screen-agent
```

It needs the `screen` extra (numpy and mss), no Bluetooth. `--monitor N` picks the monitor it
starts on (the first by default; `0` takes all of them as one picture), and the page changes it
while the agent runs: the screen mode lists the agent's monitors and the choice goes back to it.
The same list shows for the service's own screen while no agent feeds it. Both agents can run
side by side.

```bash
pip install "chsmartbulb[screen] @ git+https://github.com/mertemr/chsmartbulb"
```

### Socket protocol

One JSON object per line in each direction. Replies carry `"ok"` and, on failure, `"error"`.

```json
{"cmd": "color", "color": "#ff0064", "brightness": 0.5, "fade": true}
{"cmd": "effect", "name": "breathe", "params": {"color": "#00ff00", "period": 3}, "duration": 60}
{"cmd": "status"}
```

Commands: `status`, `on`, `off`, `color`, `brightness`, `effect`, `native`, `stop`, `effects`,
`info`, `timers`, `timer`, `raw`, `subscribe`, `reconnect`, `monitor`.

Agents stream `audio` or `screen` blocks, which are not answered. An agent may greet first with
`{"cmd": "hello", "kind": "screen", "name": "desk", "monitors": [{"index": 1, "width": 1920,
"height": 1080}], "monitor": 1}` (an audio agent sends `kind` and `name` only); the state then
lists it under `agentInfo`. `{"cmd": "monitor", "agent": "<id>", "index": 2}` asks a screen agent
for another monitor (`0` is all of them; `"local"` addresses the service's own capture, where it
has one), and the service tells that agent `{"event": "monitor", "monitor": 2}` on its own
connection, again whenever an agent of the same name comes back.

A request may carry an `"id"` of its own choosing, which the reply repeats. After
`{"cmd": "subscribe"}`, whose reply holds the current state, the service sends
`{"event": "state", "connected": ..., "link": ..., "problem": ..., "playing": ..., "on": ...,
"color": ..., "brightness": ..., "effect": ..., "native": ..., "audio": ..., "screen": ...,
"agents": {"audio": 0, "screen": 0}, "watchers": 1}`
whenever any of it changes. A client that falls behind gets the latest state, not a backlog.

`link` is `connected`, `connecting` or `waiting` (between attempts), and `problem` is why the
last attempt failed. While the bulb is away the rest of the state is what it will be given when
it returns. `agents` counts the connected agents of each kind and `watchers` the subscribed
clients. `reconnect` makes the service try at once instead of waiting out its retry delay.

`effects` lists each effect with its default `params` and a `schema` per parameter
(`{"type": "number", "min", "max", "step"}`, `{"type": "color", "optional"}`,
`{"type": "colors"}` or `{"type": "steps", "most", "easings"}`), plus the names and speed range of the bulb's `native` effects. A front end
can build its controls from that reply alone.

A network connection must start with `{"cmd": "auth", "token": "..."}`, unless the service runs
with `--no-token`. An agent then sends
`{"cmd": "audio", "levels": [bass, mid, treble], "onset": 2.4, "balance": -0.2}` for each analysed
block; those are not answered. `onset` is how far the bass stands above its recent average and
`balance` runs from -1 (left) to 1 (right); both are left out when there is nothing to report.
A screen agent sends `{"cmd": "screen", "color": "#rrggbb"}` whenever the colour changes.

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
| `custom` | Your own colours, timings and fades | `steps`, `speed` |
| `music` | Flash on the beat | `color`, `decay`, `sensitivity`, `delay` |
| `spectrum` | Bass, mids and treble as red, green and blue | `release`, `delay` |
| `volume` | One colour, as bright as the sound is loud | `color`, `release`, `floor`, `delay` |
| `stereo` | Blend two colours by where the sound sits | `left`, `right`, `width`, `release`, `delay` |
| `beathue` | Turn the colour on every beat, glow with the bass | `step`, `decay`, `floor`, `saturation`, `delay`, `sensitivity` |
| `tempo` | Warm for slow music, cool for fast, by the gaps between beats | `slow`, `fast`, `smoothing`, `delay`, `sensitivity` |
| `centroid` | Blend two colours by whether the sound is bass-heavy or bright | `low`, `high`, `width`, `release`, `delay` |
| `drop` | Open up as the music builds, flash when it comes back in | `color`, `flash`, `build`, `delay`, `sensitivity` |
| `ambient` | A calm colour that breathes, an accent on the beats | `base`, `accent`, `period`, `decay`, `delay`, `sensitivity` |
| `screen` | Follow the colour of the screen | `smoothing`, `saturation`, `white`, `balance` |
| `screensound` | The colour of the screen, as bright as the sound is loud (`chsmartbulbd` only) | `smoothing`, `saturation`, `white`, `balance`, `floor`, `release`, `shift`, `delay` |

```bash
chsmartbulb effect breathe -c 00ff00 -p 3
chsmartbulb effect palette -s colors=red,ff8000,blue -s hold=5
chsmartbulb effect music -b 60
```

`custom` plays a list of steps in a loop. Each step fades into its colour, then holds it:

```bash
chsmartbulb effect custom -s speed=2 -s steps='[
  {"color": "ff0000", "hold": 2},
  {"color": "0040ff", "hold": 1, "fade": 3, "ease": "ease-in-out"}
]'
```

Only `color` is required; `hold` defaults to 1 second and `fade` to 0. `ease` shapes the fade:
`linear`, `ease-in`, `ease-out` or `ease-in-out`. `speed` multiplies the pace of the whole
sequence, and there can be up to 16 steps. The web interface has an editor for it under Patterns
and keeps the last settings of every effect in the browser.

### Sound-reactive effects

`music`, `spectrum`, `volume`, `stereo`, `beathue`, `tempo`, `centroid`, `drop` and `ambient`
analyse the audio on the computer, taken from the monitor of the default output. It does not matter
where the sound plays: laptop speakers, headphones, another Bluetooth device or the bulb itself. They need the `audio` extra (numpy) and
the `parec` tool that comes with PulseAudio and PipeWire.

`music` changes hue on every beat unless it is given a colour. `sensitivity` (0 to 1, default 0.5)
sets how easily a rise in the bass counts as a beat: lower it when the light flashes on more than
the beat, raise it for quiet or bass-light music.

```bash
chsmartbulb effect music -c ff0080 -s sensitivity=0.3
```

`volume` ignores beats and follows the loudness in a single colour; `floor` keeps some light in
the quiet parts.

`stereo` shows `left` when the sound is on the left, `right` when it is on the right and a mix in
between. Most music sits close to the middle, so `width` stretches the measured position; raise it
if the colour barely moves, lower it if it only ever shows the two ends.

```bash
chsmartbulb effect stereo -s left=00ffff -s right=ff00ff -s width=6
```

`beathue` turns the colour by `step` degrees on every beat and never falls below `floor` of its
brightness, where `music` goes dark between beats.

`tempo` estimates the speed of the music from the gaps between beats and shows `slow` bpm as a warm
colour and `fast` bpm as a cool one. It is only as good as the beat detection: music without a clear
beat leaves it at the last tempo it heard. Brightness follows the loudness.

`centroid` shows `low` for bass-heavy sound and `high` for bright sound; `width` stretches the
measured position, as it does for `stereo`. Brightness follows the loudness, and `release` is how
fast it falls.

`drop` opens up slowly as the music gets louder and flashes in `color` for `flash` seconds when
loud music comes back in after a lull, then settles to a glow. It remembers a lull for 30 seconds,
so a build-up of up to that long still ends in a flash, and it goes dark in long silence. Its
thresholds were tuned on synthetic signals, so on some music it flashes too often or not at all;
`sensitivity` changes which beats count.

`ambient` is a calm `base` colour breathing every `period` seconds, with `accent` flashing on each
beat. In silence it keeps breathing.

```bash
chsmartbulb effect tempo -s slow=70 -s fast=140
chsmartbulb effect ambient -s base=ff6e14 -s accent=ffffff
```

`--audio-device SOURCE` picks a different source, for example the monitor of one particular
output (`pactl list short sources` shows the names).

`--mic` listens to the microphone instead, for music that plays on something the computer cannot
tap: a record player, a television, a phone. It works for the service and for the audio agent
alike, and `--audio-device` then names an input rather than an output.

```bash
chsmartbulb --mic daemon
chsmartbulb --host laptop.local --token SECRET --mic audio-agent
```

A microphone also hears the room, so the steady noise of the room is estimated and taken off:
the light reacts to what stands out of it. That estimate needs a second or two after starting,
and music that never pauses slowly raises it, which costs some of the quiet passages. `stereo`
needs a stereo microphone to show anything but the middle.

The capture hears the sound before a Bluetooth speaker or headphones play it, so the light runs
ahead of what you hear. `delay` holds the light back by that many seconds (up to 2):

```bash
chsmartbulb effect music -s delay=0.2
```

Bluetooth outputs usually need 0.15 to 0.3; wired outputs need none.

This is separate from the bulb's own `native music` mode, which reacts to sound played through
the bulb's speaker.

### Following the screen

`screen` shows the colour of what is on a monitor. The bulb is a single light, so the whole
picture becomes one colour; colourful areas count for more than grey ones, or most pictures would
come out a dull white.

```bash
chsmartbulb effect screen -s saturation=2 -s smoothing=0.4
```

The capture checks the picture 15 times a second and slows to a third of that while the picture
stands still, speeding up again at the first change; on one machine this took the capture of a
2560×1440 monitor from about 11 % to about 4 % of a core.

`smoothing` is how many seconds the light takes to follow a change and `saturation` multiplies the
colourfulness (1 leaves it as on screen). The grey part of the colour goes to the white LEDs,
because the bulb's red, green and blue together make a blue-violet instead of a white; `white=0`
turns that off.

The green LEDs are much weaker than the red and blue ones, so mixed colours drift: yellow comes
out orange, cyan comes out blue. `balance` (0 to 1, off by default) weakens red and blue to
compensate. Pure red, green and blue keep their brightness; mixed colours get dimmer.

```bash
chsmartbulb effect screen -s balance=1
```

The service follows its own screen unless a [screen agent](#the-screen-from-another-machine) is
connected. Capture goes through mss, which works on Windows, macOS and X11 but not on Wayland.

`screensound` follows the screen and the sound together: the colour is that of `screen`, the
brightness rises with the loudest band and falls back to `floor` at `release` per second, and the
hue turns by up to `shift` degrees (60 at most), one way for bass-heavy sound and the other for
bright sound. Only `chsmartbulbd` has it; the Python service does not.

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

# custom sequence: (colour, hold seconds, fade-in seconds, easing of the fade)
alarm = effects.sequence([(Color(r=255), 0.3), (Color(), 0.3), (Color(b=255), 1.0, 0.5, "ease-out")])

# anything else
def flicker(t: float) -> Color:
    return Color(r=255, g=int(80 + 60 * abs((t * 3) % 2 - 1)))
```

Building blocks: `solid`, `breathe`, `hue_cycle`, `pulse`, `strobe`, `candle`, `palette`, `fade`,
`sequence`, `custom`, `dimmed`. `catalog.create(name, params)` builds one from plain data.

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
chsmartbulb.screen         screen capture reduced to one colour, for the screen effect
chsmartbulb.catalog        effects by name with plain parameters
chsmartbulb.service        background service and its socket protocol
chsmartbulb.web            the web interface's files and WebSocket, standard library only
chsmartbulb.client         requests to a running service, and the audio and screen agents
```
