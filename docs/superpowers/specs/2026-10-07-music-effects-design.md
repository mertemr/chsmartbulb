# Five more music effects

## Intent

The sound-reactive effects (`music`, `spectrum`, `volume`, `stereo`) each show one thing about the
sound: the beat, the bands, the loudness, the position. Success: five more that show something
else, so a listener can pick a look to suit the music, and each behaves the same whether the
service runs on the Python analysis or the Rust core. Assumed (from the conversation): the
analysis is not changed, the web page needs no code because it draws its controls from the
catalog's schema, and the five are `beathue`, `tempo`, `centroid`, `drop` and `ambient`.

## What every effect reads

Only what an `AudioSource` already publishes: `levels` (`bass`, `mid`, `treble`, 0..1, and
`balance`), `beats` (a count), `last_beat` and `clock()`. Nothing is added to `Analyzer`, `Levels`,
the PyO3 module or the agent protocol, so an agent's feed drives the new effects like the old.

All five take `delay` and, where they use beats, `sensitivity`, checked as `music_pulse` does
(`delay` within `0..MAX_DELAY`, `sensitivity` within `0..1`, otherwise `ValueError` / `Err`). All
are `needs="audio"`. Like the existing effects they use `t` for how long a frame took (falls and
smoothing) and `source.clock()` for the age of a beat. A level under `DARK` (1/255) is `OFF`,
because `scaled()` never rounds a lit channel to zero; `ambient` is the one that never goes dark.

## The effects

**`beathue`**: the colour turns on every beat, the brightness follows the beat and the bass.
- hue = `beats * step` degrees (mod 360), saturation `saturation`.
- `level = floor + (1 - floor) * max(exp(-decay * (clock - last_beat)), 0.6 * bass)`.
- Params and defaults: `step` 47 (range 1..180, step 1), `decay` 5.0, `floor` 0.1, `saturation` 1.0,
  `delay` 0, `sensitivity` 0.5. Differs from `music` by the floor (never fully dark between beats
  while the music plays) and by a chosen step; `music` stays as it is.

**`tempo`**: the colour follows how fast the music is, the brightness how loud.
- On each new beat, `interval = last_beat - previous_last_beat`. Only 0.25..1.5 s (40..240 bpm)
  counts; anything else is ignored, so a missed or doubled beat does not move the estimate far.
  Estimate = `0.7 * estimate + 0.3 * interval` (the first valid interval is taken whole). With no
  beat the last estimate is kept. Before any, the position is 0.5. When the beat count has risen
  by `k > 1` between two frames, the interval is divided by `k`, so two beats are not read as one
  slow one.
- `position = clamp((bpm - slow) / (fast - slow), 0, 1)`, approached with a time constant of
  `smoothing` seconds. Colour: `(255, 60, 0)` at 0 mixed to `(0, 160, 255)` at 1.
- Brightness: loudness (the largest band) that falls at 3 per second, squared, as `volume` does.
- Params: `slow` 80 and `fast` 160 (range 40..240, step 1; `slow < fast` required), `smoothing` 2.0,
  `delay`, `sensitivity`.

**`centroid`**: the colour follows where the sound's weight lies between bass and treble.
- `c = (0.5 * mid + treble) / (bass + mid + treble)` for a nonzero sum, else the last value is kept.
  `position = clamp(width * c, 0, 1)`, smoothed with the 0.15 s constant `stereo` uses.
- Colour: `low` at 0 mixed to `high` at 1. Brightness as `stereo`: loudness with `release`, squared.
- Params: `low` red, `high` blue, `width` 2.0 (the existing range 1..10: music's centre of weight
  seldom passes a half, so it is stretched), `release` 3.0, `delay`.

**`drop`**: it opens up while the music builds and flashes when the music drops back in.
- `energy` = largest band. Two averages of it, `fast` (time constant `build` seconds) and `slow`
  (8 s), both with `1 - exp(-step / tau)`.
- A *lull* is `fast < 0.35 * slow` with `slow > 0.05`. It sets `lulled` once it has lasted 0.5 s.
- `lulled` also clears, with no flash, once the lull is over and `fast >= slow`: the music came back
  gently, so a beat minutes later is not a drop.
- A *drop* is a new beat while `lulled` and `energy >= 0.6`. It clears `lulled` and starts the
  flash: for `flash` seconds the light alternates `color` at full and dark at 8 Hz, then returns.
  A beat that comes with several others between two frames still counts once.
- Outside the flash: `level = 0.15 + 0.6 * fast`, and `color` scaled by it; `OFF` below `DARK`.
- Params: `color` white, `flash` 0.4 (0.1..2, step 0.05), `build` 3.0 (0.5..10, step 0.5), `delay`,
  `sensitivity`. These thresholds are first guesses from synthetic tests; they may need tuning
  against real tracks, which a test cannot do.

**`ambient`**: a calm colour that breathes, with an accent colour on the beats; it never goes dark.
- `base_level = 0.25 + 0.25 * (0.5 - 0.5 * cos(2 pi t / period))`, so 0.25..0.5.
- `flash = exp(-decay * (clock - last_beat))`; the frame is `base.scaled(base_level).mix(accent, flash)`.
  In silence `flash` falls to 0 and only the breathing is left.
- Params: `base` `(255, 110, 20)` (the warm colour), `accent` white, `period` 6.0, `decay` 5.0,
  `delay`, `sensitivity`.

## Where the code goes

- `src/chsmartbulb/music.py`: five builders next to `music_pulse`, sharing `_set_delay`, a new
  `_check_sensitivity` (the check `music_pulse` does inline) and `_loudness`.
- `crates/core/src/audio.rs`: the same five, with the same constants, next to `music_pulse`.
- `catalog.py` and `crates/core/src/catalog.rs`: five entries, in this order after `stereo`.
  New slider ranges for the new parameter names: `step` 1..180 / 1, `slow` and `fast` 40..240 / 1,
  `flash` 0.1..2 / 0.05, `build` 0.5..10 / 0.5. `smoothing` already has 0..2 / 0.05. The `Sources`
  / audio plumbing needed for `needs="audio"` is already there.
- `docs/usage.md`: five rows in the effect table, and a paragraph on `tempo`, `drop` and their limits.
  `README.md` if it lists the effects.
- No change in `web/` or the tracked bundle; no change in `crates/python`.

## Errors

A parameter out of range raises on creation, so a bad `effect` request is refused up front as for the
other effects: `slow >= fast`, `step` outside 1..180, `floor` or `saturation` outside 0..1, `width`
or `period` not positive, `flash` or `build` not positive.

## Testing

Python (`tests/test_catalog_and_music.py`) and Rust (`audio.rs` with the manual clock) each get the
same scenarios on a scripted source, asserting colours the way the `stereo` tests do:
- `beathue`: hue steps by `step` on a beat, falls to `floor` between beats, silent source shows `floor`.
- `tempo`: a source beating at 0.5 s (120 bpm) settles to the middle colour with `slow=80, fast=160`;
  a 0.1 s interval is ignored; the estimate holds without beats.
- `centroid`: bass only shows `low`, treble only shows `high`, an even mix lies between; position
  holds in silence.
- `drop`: a long quiet stretch followed by a beat at high energy flashes; the same beat in steady
  loud music does not; the flash ends after `flash` seconds.
- `ambient`: silent source breathes between 0.25 and 0.5 of `base`, a beat shows `accent` and then
  returns.
- The catalog: `needs` lists the five as `audio`, a missing source and every invalid parameter above
  are refused, the schema carries the new ranges, and the Python and Rust catalogs list the same names.

Done when `ruff check`, `pytest`, `cargo fmt --check`, `cargo clippy -D warnings` and `cargo test`
pass.
