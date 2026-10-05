# The app

`app/` is a Tauri 2 app for Android, Linux and Windows. It shows the same interface as the web
page the Python service serves, and carries its own service: a Rust port of the Python one in
[`crates/core`](../crates/core). Nothing else has to run; the phone or computer talks to the
bulb itself.

```
web/ (Svelte)  ── Link ──┬─ WebSocket ─ Python service (browser, or the app's "use a computer's service")
                         └─ Tauri IPC ─ crates/core (Rust service, in the app)
                                           │ Connector / Link
                     app/plugin ───────────┼──────────────────────────────┐
                     Android (Kotlin)      Linux                          Windows
                     BLE GATT, SPP         BLE (btleplug), RFCOMM socket  BLE (btleplug), Winsock RFCOMM
```

The Rust service answers the same JSON requests and sends the same state events as
`chsmartbulb.service`, so the interface does not know which one it talks to. Its tests mirror
the Python service's against an in-memory bulb (`cargo test`).

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

The desktop app does not listen to sound yet, and the screen effect needs a computer: for
those, run the Python service and connect the app to it from its first screen.

## Background

While a bulb is connected, Android shows a quiet notification. It keeps the app running, so
effects keep playing with the screen off.

## Building

The APK, the Linux bundles (AppImage, deb, rpm) and the Windows installers are built by the
[`app` workflow](../.github/workflows/app.yml) on every push and attached to the run. An APK
signed with a key from the repository's secrets (`ANDROID_KEYSTORE`, base64, and
`ANDROID_KEYSTORE_PASSWORD`) updates in place; without them each run signs with a new key, so
an installed build has to be removed before installing a newer one.

Locally, with Rust and Node 22:

```bash
npm ci --prefix web
npm ci --prefix app
cd app
npx tauri dev            # desktop, with hot reload
npx tauri build          # desktop bundles in target/release/bundle
```

The Linux build needs WebKitGTK 4.1, e.g. on Arch
`pacman -S webkit2gtk-4.1 libayatana-appindicator librsvg base-devel`, on Debian and Ubuntu
`apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libdbus-1-dev`.

For Android, with the Android SDK and NDK installed (`ANDROID_HOME`, `NDK_HOME`) and the
Rust targets added (`rustup target add aarch64-linux-android armv7-linux-androideabi
x86_64-linux-android i686-linux-android`):

```bash
cd app
npx tauri android init   # generates src-tauri/gen/android, not kept in the repository
npx tauri icon icon.svg
npx tauri android dev    # on a connected phone
npx tauri android build --apk
```

The Kotlin side is in [`app/plugin/android`](../app/plugin/android); its manifest brings the
Bluetooth, microphone and foreground service permissions into the app.
