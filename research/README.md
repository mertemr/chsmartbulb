# Research material

Scripts and raw output from reverse engineering the bulb. Kept for reference; none of it is part
of the library. See [docs/research.md](../docs/research.md) for what each experiment showed.

Set the address of your bulb where a script has `AA:BB:CC:DD:EE:FF`.

| Script | Purpose |
|---|---|
| `sdp_dump.py` | Raw SDP query over L2CAP |
| `gatt_dump.py` | Read-only GATT discovery over a raw LE ATT socket |
| `probe.py` | Send hex frames over RFCOMM and print the answers |
| `rig.py` | Shared helper: RFCOMM link plus webcam colour probe |
| `calib.py` | Pick camera exposure and the measurement region |
| `exp_run.py` | Light commands with a steady-state camera reading |
| `exp_fx.py` | Light commands with a camera time series |
| `exp_session.py` | Hello, latency, command rate, idle, status frame |
| `exp_music.py` | Sound-reactive mode |
| `exp_speed.py` | Effect period against the speed byte |
| `ble_dbus.py`, `ble_dbus2.py` | Frames over the BLE characteristics through BlueZ |
| `lib_check.py`, `ble_lib_check.py` | Physical check of the library over RFCOMM and BLE |

`results/exp.jsonl` holds every frame sent and received; the `.txt` files are the script outputs.
