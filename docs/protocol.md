# Protocol

Control protocol of the CHSmartBulb / BL08A speaker bulb, worked out on a real device
(advertised name `SmartBulb Bluetooth`, firmware `1.0.`, model `BL04`).

Every statement carries a tag saying how it was established:

| Tag | Meaning |
|---|---|
| **[M]** | Measured: the light output was recorded with a camera |
| **[R]** | Read: confirmed by an answer from the device |
| **[C]** | Seen only in third-party captures or code, not tried on this device |
| **[?]** | Unknown |

Raw experiment output lives in [`research/results/`](../research/results); how the
experiments were run is described in [research.md](research.md).

## Transport

The same frames work over two bearers.

### Bluetooth Classic: SPP over RFCOMM

- **[R]** Serial Port service (`0x1101`) on RFCOMM channel **2**.
- **[M]** Works while the bulb is also connected as an A2DP speaker.
- **[R]** Reopening the channel right after closing it fails with `EBUSY` for about half a second.

### Bluetooth Low Energy: GATT

| Service | Characteristic | Handle | Role |
|---|---|---|---|
| `0x7777` | `0x8877` | `0x0011` | Command input |
| `0x6666` | `0x8888` | `0x000d` (CCCD `0x000e`) | Answer output (notifications) |

- **[M]** Frames written to `0x8877` **with response** are executed.
- **[M]** Writes without response are ignored. Writing to `0x8888` does nothing.
- **[R]** Answers arrive as notifications on `0x8888`. Long answers are split across several
  notifications and must be reassembled using the length field.
- **[R]** One write takes about 75 ms, so BLE carries at most ~13 commands per second.

### One bearer at a time

**[R]** The bulb accepts a single bearer. While a Classic link is up it does not advertise over LE;
while an LE link is up a Classic connection attempt times out. Use RFCOMM when the bulb is also
your speaker, BLE when you only want the light (phones included).

### Session

- **[M]** The vendor app sends the ASCII string `01234567` after connecting. It is not required.
- **[M]** No keep-alive is needed; a command still worked after 25 s of silence. The app polls
  query `0x00` once a second, which is only a status poll.
- **[R]** This dialect answers query `0x02` with `47 0c`.

## Frame format

```
01 fe 00 00 | TT | CC | LL LL | body
```

| Field | Size | Description |
|---|---|---|
| magic | 4 | `01 fe 00 00` **[R]** |
| `TT` type | 1 | `0x51` `Q` query, `0x53` `S` set, `0x41` `A` answer **[R]** |
| `CC` command | 1 | An answer repeats the command byte of its query **[R]** |
| `LL LL` length | 2 | Length of the **whole frame**, little-endian **[R]** |
| body | LL-8 | Command specific |

There is no checksum **[R]**. `S` frames are not acknowledged **[R]**.

## Queries (`TT = 0x51`)

All read-only, all run on the device.

| `CC` | Body sent | Answer body | Meaning |
|---|---|---|---|
| `0x02` | `00 00 00 80 00 00 00 80` | `47 0c 00 00 00 00 00 00` | Dialect / model id **[R]** |
| `0x80` | 8 × `00` | 8 × `00`, `"1.0."`, `00 00 00 03`, name (zero padded) | Version and device name **[R]** |
| `0x81` | 8 × `00` | `7c 58 00 00 34 30 4c 42 01 01 00 00 1a 00` | Bytes 0-1 per-device id, bytes 4-7 `"BL04"` reversed, last two bytes change on every query **[R]** / **[?]** |
| `0x82` | 8 × `00` | `GG BB RR 00 00 WW YY FF` | Light state, see [read-back](#read-back) **[R]** |
| `0x00` | `00 00 00 80 00 00 00 80` | 8 × `00` + 24 bytes | Status, see [below](#status-frame) **[?]** |
| `0x30` | `00 00 00 80 00 00 00 80` | count, records | Timers, see [below](#timers) **[R]** |
| `0x31` | `00 00 00 80 00 00 00 80` | contains `alarm1.mp3` | Alarm ringtone list **[R]**, fields **[?]** |
| `0x32` | `ff ff ff ff 00 00 00 80` | 4 × 32 bytes | Alarms with GBK names **[R]**, fields **[?]** |

```
TX 01fe0000 51 82 1000 0000000000000000
RX 01fe0000 41 82 1000 0000ff0000000000        red is on
```

## Light command (`53 83`, length `0x10`)

```
01 fe 00 00 53 83 10 00 | GG BB RR | SP | EF | WW | YY | FD
```

| Byte | Field | Notes |
|---|---|---|
| 0 | `GG` green | 0..255 **[M]** |
| 1 | `BB` blue | 0..255 **[M]** |
| 2 | `RR` red | 0..255 **[M]** |
| 3 | `SP` speed | Effect dependent, see [speed](#speed-byte) **[M]** |
| 4 | `EF` effect | `0x50` = fixed colour, see [effects](#built-in-effects) **[M]** |
| 5 | `WW` white LEDs | 0..255 **[M]** |
| 6 | `YY` "yellow" | Produces no light on this model **[M]** |
| 7 | `FD` fade | 0 = instant, non-zero = ~1 s transition **[M]** |

```
red            01fe000053831000 00 00 ff 00 50 00 00 00
green          01fe000053831000 ff 00 00 00 50 00 00 00
blue           01fe000053831000 00 ff 00 00 50 00 00 00
white          01fe000053831000 00 00 00 00 50 ff 00 00
off            01fe000053831000 00 00 00 00 50 00 00 00
fade to blue   01fe000053831000 00 ff 00 00 50 00 00 01
```

### Behaviour

- **[M] Channel order is green, blue, red.**
- **[M] There is no brightness field; brightness is the channel value.** Red output in camera
  units: 255 → 75.6, 128 → 42.1, 64 → 20.9, 32 → 9.6, 16 → 5.2, 4 → 2.8, 1 → 1.5.
- **[M]** RGB and white LEDs can be lit together.
- **[M]** The yellow byte lights nothing, alone or with white. It is stored and read back,
  so it probably drives warm-white LEDs on other models. This bulb has no colour temperature.
- **[M] There is no power command.** Off is all channels at zero.
- **[M] An unknown effect byte makes the bulb ignore the whole frame.** Colours sent with
  `EF` = `0x00`, `0x4f`, `0x60`, `0x63`, `0x70` or `0xff` were not applied.
- **[M]** `FD` values 1, 2 and `0xff` all give the same ~1 s linear fade from the old colour.
  Third-party code calls this byte "auto close"; on this device it is a fade flag.
- **[M]** With a fixed colour the speed byte has no effect.

### Timing

- **[M]** Command to visible change: about 0.1 s over RFCOMM (includes camera latency).
- **[M]** The bulb follows colour changes up to about 25 per second. Faster streams do not queue
  up; the bulb ends on the last colour sent.

### Read-back

Query `0x82` answers `GG BB RR 00 00 WW YY FF`.

- **[R] Channels are rescaled so that the largest reads 255.** Writing `56 34 12 .. 78` reads back
  `b6 6e 26 .. ff`. The colour mix and on/off state can be read; **brightness cannot**.
- **[R]** Speed and effect always read `00`, so a running effect cannot be detected.
- **[?]** The last byte is normally `00`; it read `04` after the undocumented effect `0x5b`.

## Built-in effects

All recorded as a time series with the camera **[M]**. "Colour" says whether the effect uses the
colour in the frame. Any light command ends a running effect.

| `EF` | Name in the app | Colour | Observed |
|---|---|---|---|
| `0x50` | Fixed | yes | Steady colour |
| `0x51` | KTV / music | no | Dark in silence; flashes a new colour on each beat of audio played through the bulb |
| `0x52` | Breathing | yes | Swells and fades |
| `0x53` | Rainbow | no | Slow hue cycle |
| `0x54` | Flash | yes | Hard blinking |
| `0x56` | Heartbeat | yes | Soft pulse |
| `0x58` | Automatic | no | Smooth cycle through colours |
| `0x5a` | Candlelight | yes | Irregular flicker |
| `0x5c` | Ocean | no | Blue and green waves |
| `0x5d` | Natural | no | Swings between blue-green and reddish |
| `0x5e` | Sunset | no | Mostly red, drifting to purple and yellow |
| `0x5f` | Passion | no | Red and yellow pulse |
| `0x61` | RGB-Cut | no | Steps through red, green, blue |

### Speed byte

The byte holds two fields: the **high nibble** sets ramp time, the **low bits** set hold time.
Larger is slower.

- **[M] Breathing**: period ≈ 0.25 s + 0.128 s × (high nibble + low nibble) for low nibble 0..3.
  Measured from 0.25 s (`0x00`) to 2.57 s (`0xf3`). The model breaks down for larger low nibbles **[?]**.
- **[M] Flash**: depends only on the low bits, period ≈ 0.133 s × (low + 1).
- **[M] Heartbeat**: high nibble only, 0.25 s (`0x00`) to 2.17 s (`0xf0`).
- **[C]** For a speed level `i` in 0..15 the vendor app sends `(i << 4) | (i >> 2)`, or `i << 4`
  for heartbeat. It uses a raw 0..30 range for KTV and 0..60 for candlelight; their effect was
  not measured **[?]**.

### Undocumented effect bytes

The bulb reacts to these although the app was not seen using them with a colour **[M]**. Their
behaviour depends on the previous state, so the library does not expose them.

| `EF` | Observed |
|---|---|
| `0x55` | Oscillates between the previous and the new colour |
| `0x57` | Fades from the previous colour to the new one in ~0.6 s |
| `0x59` | Ignored after a fixed colour; the app sends it with zero colour **[C]** |
| `0x5b` | Switches instantly; read-back flag byte becomes `04` |
| `0x62` | Colour cycle similar to `0x58` |

## Timers

Query `0x30` answers with a count (u32 LE), four zero bytes, then one 44-byte record per timer:

```
name[32]                                  C string
II F1 EN DM HH MM 00 03 01 00 00 00
```

| Field | Meaning |
|---|---|
| `II` | Index **[R]** |
| `F1` | Always `01` when read **[?]** |
| `EN` | Enabled **[R]** (written and read back) |
| `DM` | Day mask, `7f` = every day **[C]** |
| `HH MM` | Hour and minute **[C]** |
| rest | `00 03 01 00 00 00` **[?]** |

A timer is written with `53 30`, length `0x3c`:

```
01 fe 00 00 53 30 3c 00 | index (u32 LE) | enabled (u32 LE) | name[32] | II 00 EN DM HH MM 00 03 01 00 00 00
```

- **[R]** Disabling a stored timer this way worked and persisted across connections.
- **[R]** The write is not acknowledged and does not change the light.
- **[C]** Changing the time, days or name with the same frame should work but was not tried.

## Status frame

**[?]** Query `0x00` returns 24 bytes after eight zero bytes. On the test device they never changed:

```
00 1f 00 16 00 00 00 05 00 00 02 00 00 00 00 00 00 04 88 11 02 10 00 00
```

Not affected by colour, effect, playback or host volume. Other people's captures show bytes 0, 3,
20 and the last two varying **[C]**.

## Commands not sent

These change persistent settings and were left alone. Their layout comes from captures only **[C]**.

| Frame | Meaning |
|---|---|
| `53 00`, body `00000000 00000080 YY YY MM DD hh mm ss 00` | Set the clock (year u16 LE). The app sends it on every connect. |
| `53 80`, length `0x4c`: 8 × `00`, `03`, name, zero padding | Rename the device |
| `50 0a`, body `00000080 00000080` | Unknown **[?]** |

## Other dialect

Bulbs sold as `i_Lamp` / "iLight pro" share the frame header but answer query `0x02` with `01 00`
and control the light through sub-commands wrapped in `0d … 0e` inside command `0x81`. That does
not apply here; this device answers `0x81` with its hardware id.

## Open questions

1. Meaning of the status frame fields.
2. Tail of the `0x81` answer and the record layout of queries `0x31` and `0x32`.
3. Purpose of effect bytes `0x55`, `0x57`, `0x59`, `0x5b`, `0x62` and of the `04` flag.
4. Speed byte behaviour for low nibbles above 3, and for KTV and candlelight.
5. The `50 0a` frame.
6. Whether clock set and rename work as captured, and whether the clock can be read.
