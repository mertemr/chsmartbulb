# The light follows the computer being away, and a lighter screen capture

## Intent

The computer that holds the bulb should not leave it lit, or its capture running, while nobody is
there. Success: locking the screen dims the light, sleeping or shutting down turns it off, and
coming back restores what it showed (colour, brightness, effect). Following the screen costs less
when the picture does not move. Assumed (from the conversation): the bulb is held by this computer
over BLE (the daemon runs here); lock dims, sleep and shutdown turn off; nothing is on by default.

## Away and back (`service.py`)

- `{"cmd":"away","reason":"lock"|"sleep"|"shutdown"}` and `{"cmd":"back"}`, ordinary commands, so a
  presence watcher on another machine could send them too.
- The service keeps the **plan untouched** and a separate `_away` (the reason, or `None`). Away stops
  the running effect (its capture stops with it) and shows the away look on the bulb without writing
  it into the plan: `off` turns the bulb off, `dim` sets brightness to 10 %. Back clears `_away` and
  applies the plan again, which is the whole restore.
- Which look goes with which reason is the service's `away_looks`, e.g. `{"lock": "dim", "sleep": "off",
  "shutdown": "off"}`; a reason without a look is ignored. A later `away` replaces an earlier one (lock,
  then sleep: off). `back` without `away` does nothing.
- Anything that changes the plan (a request from a page, an effect) ends the away state: a person
  using the light wins. A reconnect while away shows the away look again instead of the plan.
- State gains `"away": <reason or null>` so pages can say why the light is dark.

## Presence watcher (`presence.py`, Windows)

A hidden message-only window on its own thread, ctypes only. It registers for session notifications
and reports `lock`, `unlock`, `sleep` (`PBT_APMSUSPEND`), `resume` (`PBT_APMRESUMEAUTOMATIC` and
`PBT_APMRESUMESUSPEND`) and `shutdown` (`WM_ENDSESSION`) to a callback on the event loop. On other
systems it does nothing. The daemon maps them: lock/sleep/shutdown to `away`, unlock/resume to `back`.
Sleep gives the machine about a second; the away look is sent at once, and if the link has not
finished writing, the bulb is simply restored by the plan after resume.

## Daemon options (`cli.py`)

`--on-lock dim|off|none`, `--on-sleep off|dim|none` (also used for shutdown); both default `none`, so
nothing changes for anyone who does not ask. The watcher runs only when one of them is set.
Starting at logon is a scheduled task running the daemon (documented in `docs/usage.md`).

## Lighter screen capture (`screen.py`)

The capture thread slows down while the picture stands still: after the colour has not changed for a
second it waits up to five frame times, and a change brings the full rate back at once. The pacing is a
pure function (`pace(still_frames)`), tested without a screen. Stopping the capture while away comes
from the service stopping the effect. The cost before is measured on this machine (about 10.8 % of a
core for the local capture of a 2560×1440 monitor at 15 Hz) and again afterwards.

Not in this change: GPU-side downscaling (a separate Windows capture backend), the Rust service
(it has no capture of its own and its app has no lock/sleep hooks yet), the startup installer as code.

## Testing

Python: away/back with each look, restore of colour, brightness and effect, a request ending away,
reconnect while away, `back` without `away`, the watcher's message mapping with a fake window
procedure, the pacing function, the daemon options.
