# The app

`app/` is a Tauri 2 app for Android, Linux and Windows. It shows the same interface as the web
page `chsmartbulbd` serves, and carries the same service inside it:
[`crates/core`](../crates/core). Nothing else has to run; the phone or computer talks to the
bulb itself.

```
web/ (Svelte)  ── Link ──┬─ WebSocket ─ chsmartbulbd (browser, or the app's "use a computer's service")
                         └─ Tauri IPC ─ crates/core (the service, in the app)
                                           │ Connector / Link
                     app/plugin ───────────┼──────────────────────────────┐
                     Android (Kotlin)      Linux                          Windows
                     BLE GATT, SPP         BLE (btleplug), RFCOMM socket  BLE (btleplug), Winsock RFCOMM
```

Either way it is the one service, answering the [socket protocol](usage.md#socket-protocol), so
the interface does not know which it talks to. Its tests run against an in-memory bulb
(`cargo test`).

## BLE or Classic

The bulb takes one Bluetooth bearer at a time (see [device.md](device.md)). The app asks which
one when the bulb is chosen:

| | BLE (default) | Classic (SPP) |
|---|---|---|
| Use when | The light only; the sound plays on the phone or another speaker | The bulb is also the speaker |
| Needs | No Classic link to the bulb: switch off *Media audio* for it in the Bluetooth settings, or do not pair it as a speaker | The bulb paired with the phone |
| Colour changes | about 13 a second | about 25 a second |

Over BLE the bulb does not answer while a Classic link is up, which the app says when it
cannot connect. *Change bulb* under Device goes back to the choice.

## Sound

The effects under *Sound* follow what the app hears. On Android it listens to either:

- **What this device plays**: Android 10's playback capture, wherever the sound goes (phone
  speaker, headphones, any Bluetooth speaker). Android asks once per session to share the
  audio. Apps that opt out of capture (some streaming apps do) cannot be followed; use the
  microphone for those.
- **The microphone**: the room, with its steady noise taken off.

Only band levels and beats are computed from the sound; it is not stored or sent anywhere.
A Bluetooth speaker plays about 0.2 s after the capture hears the sound, so raise an effect's
*delay* until the light hits with the beat.

Where the phone's sound goes is the system's choice, not the app's. On Samsung phones
*Separate app sound* sends one app's sound to another device, and *Dual audio* plays on two
Bluetooth speakers at once. The *Sound* panel shows the current outputs and links to the
settings.

The desktop app listens to what the computer plays, or to its microphone: on Linux through
`parec` from the default output's monitor (PipeWire and PulseAudio both offer it), on Windows as
a WASAPI loopback of the default output. The screen effect follows a computer's screen through
the Python package's `screen-agent`, with sharing turned on (see below).

## Sharing on the network

*Share on the network* under Device makes the app a service for other machines, as
`chsmartbulbd --listen 8377 --web 8378` does: browsers open the interface on port 8378,
and the Python package's command line and its `audio-agent` and `screen-agent` connect to port
8377 with the token shown there. A phone holding the bulb can follow a computer's screen or
sound this way. Nothing is encrypted, so keep it to a network you trust; *New token* turns away
every machine that knew the old one.

## While the computer is away

In the desktop app, *While you are away* under Device chooses what the light does when the
computer locks (dim or off) and when it sleeps or shuts down, as the daemon's `--on-lock` and
`--on-sleep` do; nothing changes unless asked. The light comes back as it was when the computer
does, or as soon as anything is changed. Linux follows logind on the system bus (the session's
lock hint, sleep and shutdown); Windows follows the session and power messages every window gets.
The `away` and `back` requests are part of the socket protocol, so a page or another machine can
send them too, and the interface says when the light is resting and offers to show it again.

## Without a bulb

*Try the app with a simulated one*, at the bottom of the first screen, drives an in-memory bulb
that behaves like the real one. The same simulation runs the Rust tests, and
`cargo run -p chsmartbulb-daemon -- --simulate --listen 8377 --web 8378 --no-token` serves it on
ports 8377 and 8378 for trying the web interface and the command line.

## Background

While a bulb is connected, Android shows a quiet notification. It keeps the app running, so
effects keep playing with the screen off.

## Python bindings

[`crates/python`](../crates/python) builds `chsmartbulb-native`, the Rust core for Python. With
it the `chsmartbulb` package plays the effects of the catalog itself, which exist in Rust only,
and analyses sound in Rust: the audio agent then needs no numpy and hears music exactly as the
service does (a test compares the two block by block). Without it the command line still drives
the light and the service, and the agents still run.

```bash
pip install "chsmartbulb-native @ git+https://github.com/mertemr/chsmartbulb#subdirectory=crates/python"
```

## Building

The APK, the Linux bundles (AppImage, deb, rpm) and the Windows installers are built by the
[`app` workflow](../.github/workflows/app.yml) on every push and attached to the run. An APK
signed with a key from the repository's secrets (`ANDROID_KEYSTORE`, base64, and
`ANDROID_KEYSTORE_PASSWORD`) updates in place; without them each run signs with a new key, so
an installed build has to be removed before installing a newer one.

Locally, with Rust and Node 22 and pnpm:

```bash
pnpm --dir web install --frozen-lockfile
pnpm --dir app install --frozen-lockfile
cd app
pnpm tauri dev            # desktop, with hot reload
pnpm tauri build          # desktop bundles in target/release/bundle
```

The Linux build needs WebKitGTK 4.1, e.g. on Arch
`pacman -S webkit2gtk-4.1 libayatana-appindicator librsvg base-devel`, on Debian and Ubuntu
`apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libdbus-1-dev`.

For Android, with the Android SDK and NDK installed (`ANDROID_HOME`, `NDK_HOME`) and the
Rust targets added (`rustup target add aarch64-linux-android armv7-linux-androideabi
x86_64-linux-android i686-linux-android`):

```bash
cd app
pnpm tauri android init   # generates src-tauri/gen/android, not kept in the repository
pnpm tauri icon icon.svg
pnpm tauri android dev    # on a connected phone
pnpm tauri android build --apk
```

The Kotlin side is in [`app/plugin/android`](../app/plugin/android); its manifest brings the
Bluetooth, microphone and foreground service permissions into the app.
