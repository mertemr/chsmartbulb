# Research notes

How the protocol was worked out, which experiment answered which question, and which guesses
turned out wrong. Scripts are in [`research/`](../research), raw output in
[`research/results/`](../research/results).

## Method

- **Discovery**: `bluetoothctl`, `busctl`, a raw L2CAP SDP query (`sdp_dump.py`) and a raw LE ATT
  socket (`gatt_dump.py`). `btmon` needs root, so instead every frame sent and received was logged
  to `exp.jsonl`.
- **Reference captures**: `btsnoop` logs published by other people (see the README for credits)
  were decoded with `tshark -Y btspp` and used as a source of hypotheses only.
- **Observation**: a webcam with exposure and white balance locked. The bulb was switched between
  a few colours and the pixels that changed most became the measurement region (`calib.py`). After
  each command the mean colour of that region was compared with a dark reference (`rig.py`). No
  frames were stored. At 30 fps, periods below ~0.1 s are not reliable.

## Experiments

| # | Question | Experiment | Result | Output |
|---|---|---|---|---|
| 1 | Is it a BLE device? | BlueZ object tree, SDP dump | Connected over Classic; control runs on SPP, RFCOMM channel 2 | `sdp.txt` |
| 2 | Which dialect? | Query `0x02` | `47 0c`, the BL08A dialect | `exp.jsonl` |
| 3 | Channel order? | Light one channel at a time | Green, blue, red | `calib.txt` |
| 4 | Where is brightness? | Step a channel from 255 to 1 | Output scales with the channel value; no separate field | `e1.txt` |
| 5 | Why does read-back say `ff`? | Write mixed values, read back | The bulb rescales to make the largest channel 255 | `run2.txt`, `run3.txt` |
| 6 | What is byte 6? | Write it alone and with white | Read back but produces no light | `calib.txt`, `e1.txt` |
| 7 | What is byte 4 (`0x50`)? | Time series for many values | Effect selector; unknown values drop the frame | `e4.txt`, `e5.txt`, `e9.txt` |
| 8 | What is byte 7? | Transitions with 0, 1, 2, `0xff` | Non-zero gives a ~1 s fade | `e6.txt` |
| 9 | Are the effects real? | Time series of 13 codes | Each gives a distinct pattern | `e4.txt`, `e5.txt` |
| 10 | How is speed encoded? | Period measurement | High nibble ramp, low bits hold | `e7.txt`, `espeed.txt` |
| 11 | Music mode? | Play a 2 Hz beat through the bulb | `0x51` reacts to sound, dark in silence | `em.txt` |
| 12 | Are hello and keep-alive needed? | Skip hello; idle for 25 s | Neither is needed | `es.txt` |
| 13 | How fast can commands go? | Alternate red/blue at 5-100 Hz | Followed up to ~25 Hz, no backlog | `es.txt` |
| 14 | Are the BLE services really dummies? | Write frames to `0x8888` and `0x8877` | `0x8877` takes commands, `0x8888` notifies answers | `gatt.txt` |
| 15 | Does the library work physically? | `lib_check.py` | Brightness, on/off, fade, effects and reconnect confirmed | |
| 16 | Can timers be written? | Disable a stored timer, read back | Works; the light is not affected | `exp.jsonl` |
| 17 | Does the bleak transport work? | `ble_lib_check.py` | Yes once LE is up; BlueZ will not open LE by address | `ble_lib_check.txt` |

## Wrong turns

- **"Byte 4 is brightness."** The normalised read-back suggested it. Experiment 7 showed it selects the effect.
- **"Effect `0x00` turns the light off."** It looked that way because the bulb was already dark.
  Resetting to a fixed colour before each code showed the frame is simply ignored.
- **"Status byte 3 is the volume."** The value matched the host volume by coincidence; changing
  the volume did not change the frame.
- **"The BLE services are unused."** Taken from an earlier write-up; experiment 14 showed otherwise.
- **"Channel order is blue, green, red."** Found in older code; wrong.
- **"Connecting by address with bleak is enough."** BlueZ chose the Classic bearer, reported the
  device as connected, and the first GATT write failed. The transport now does a GATT read while
  opening so this shows up as a connection error.
- Python builds downloaded by `uv` have no Bluetooth sockets, so the project is pinned to the system interpreter.
