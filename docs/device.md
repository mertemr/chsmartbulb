# Device

What the bulb exposes over Bluetooth. Collected with BlueZ 5.87 on Linux; raw dumps are in
[`research/results/sdp.txt`](../research/results/sdp.txt) and
[`research/results/gatt.txt`](../research/results/gatt.txt).

## Identity

| Property | Value |
|---|---|
| Name | `SmartBulb Bluetooth` (older units advertise `Chsmartbulb`) |
| Address prefix | `F4:4E:FD` |
| Class of device | `0x240404` (audio, headset) |
| LE manufacturer data | company `0x03EE` (CUBE Technologies), value = device address reversed |
| PnP ID | vendor `0x099A`, product `0x0500`, version `0x011B` |
| Protocol id (query `0x02`) | `47 0c` |
| Version / model | `1.0.` / `BL04` |

The chip is dual mode: it speaks both Bluetooth Classic and BLE, but accepts only one of them at a time.

## Bluetooth Classic services (SDP)

| Service | UUID | Transport |
|---|---|---|
| Audio Sink (A2DP 1.2) | `0x110B` | L2CAP PSM 25 |
| AVRCP 1.5 | `0x110E`, `0x110F`, `0x110C` | L2CAP PSM 23 / 27 |
| Handsfree 1.6 | `0x111E` | RFCOMM channel 1 |
| **Serial Port** | `0x1101` | **RFCOMM channel 2** |
| PnP Information | `0x1200` | |

The light is controlled through the Serial Port service.

## BLE services (GATT)

```
0x0001-0x0005  Service 0x1800 (Generic Access)
  0x0003  0x2a00 Device Name      read, write    "SmartBulb Bluetooth"
  0x0005  0x2a04 Conn. Parameters read
0x0006-0x000a  Service 0x1800 (second copy)
  0x0008  0x2a00 Device Name      read, write    "LE Sample Device"
  0x000a  0x2a04 Conn. Parameters read
0x000b-0x000e  Service 0x6666
  0x000d  0x8888                   read, write, write-no-rsp, notify    answers
  0x000e  0x2902 CCCD
0x000f-0x0011  Service 0x7777
  0x0011  0x8877                   read, write, write-no-rsp            commands
```

Both vendor characteristics read as a 197-byte buffer filled with `0x88` or `0x77`, and a written
value simply shows up at the start of that buffer. Earlier write-ups took them for dummies because
of this. In fact frames written with response to `0x8877` are executed and answered through
notifications on `0x8888`.

## Connection behaviour

- While a Classic link is up the bulb does not advertise over LE.
- When the Classic link drops, LE advertising starts and a GATT connection can be made.
- While an LE link is up, a Classic connection attempt times out.
- Reopening the RFCOMM channel right after closing it returns `EBUSY` for about half a second.

## BLE on Linux / BlueZ

BlueZ picks the bearer itself when asked to connect a dual-mode device, and for a device that is
bonded over Classic only it always picks Classic. `bleak` relies on that call, so on Linux
`ChSmartBulb.ble(address)` does not get an LE link to a bulb that is paired as a speaker:

| Starting state | Result of connecting by address with bleak |
|---|---|
| Nothing connected | BlueZ brings up the Classic audio link; GATT operations fail with `Not connected` |
| Classic connected | The bulb is not advertising, so the scan times out |
| LE already connected by bluetoothd | Scan times out, but passing the BlueZ object path works |

A way to get the LE link up without root is to connect a raw L2CAP ATT socket once
([`research/gatt_dump.py`](../research/gatt_dump.py)); bluetoothd then takes the LE link over and
bleak can be given a `BLEDevice` carrying the object path
([`research/ble_lib_check.py`](../research/ble_lib_check.py)).

After LE has been used, bluetoothd reconnects LE by itself whenever the link drops, which blocks
the audio connection. To go back to Classic, disconnect and connect the audio profile right away:

```bash
bluetoothctl disconnect AA:BB:CC:DD:EE:FF
```

```bash
busctl call org.bluez /org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF org.bluez.Device1 ConnectProfile s 0000110b-0000-1000-8000-00805f9b34fb
```

On Linux the practical choice is RFCOMM. The BLE transport matters on platforms without RFCOMM
access and for phones.

## Stored timers

The bulb keeps schedule entries that run on its own clock. The test unit had two left over from
the vendor app:

| Name | Index | Days | Time |
|---|---|---|---|
| `power off` | 6 | every day | 06:20 |
| `power on` | 5 | every day | 10:35 |

The clock cannot be read and is only set by the app, so an enabled timer on a bulb that has not
seen the app for a while fires at an unpredictable time. `chsmartbulb timers` lists them and
`chsmartbulb timer INDEX off` disables one.
