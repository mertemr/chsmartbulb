# Five More Music Effects Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the sound-reactive effects `beathue`, `tempo`, `centroid`, `drop` and `ambient` to both the Python package and the Rust core, with identical behaviour.

**Architecture:** Each effect is a builder next to `music_pulse` in `src/chsmartbulb/music.py` and in `crates/core/src/audio.rs`, registered in `catalog.py` and `catalog.rs`. They read only what an `AudioSource` already publishes (`levels`, `beats`, `last_beat`, `clock()`), so the analyser, the PyO3 module, the agent protocol and the web page are untouched (the page builds its controls from the catalog schema).

**Tech Stack:** Python 3.10+ (pytest, ruff), Rust 2021 (cargo test/clippy/fmt), `FakeMusic` from `tests/fakes.py`, the manual clock in `audio.rs` tests.

**Spec:** `docs/superpowers/specs/2026-10-07-music-effects-design.md`

## Global Constraints

- Analysis is not changed: no edit to `Analyzer`, `Levels`, `crates/python`, `service.py`, `web/` or `src/chsmartbulb/webui`.
- Every new effect is `needs="audio"` / `Needs::Audio`, takes `delay` (checked `0..MAX_DELAY`), and, where it uses beats, `sensitivity` (checked `0..1`).
- A frame level under `DARK` (1/255) is `OFF`; only `ambient` and the `drop` glow never go dark.
- Parameter errors are raised at creation: Python `ValueError`, Rust `Err(invalid(..))`.
- Python and Rust use the same constants and the same arithmetic, so the same input gives the same colour (Rust `round` is ties-to-even, as Python's).
- Catalog order: the five entries go after `stereo` and before `screen`, in the order `beathue`, `tempo`, `centroid`, `drop`, `ambient`.
- Slider ranges for new parameter names: `step` 1..180 / 1, `slow` and `fast` 40..240 / 1, `flash` 0.1..2 / 0.05, `build` 0.5..10 / 0.5. `smoothing`, `width`, `release`, `period`, `decay`, `floor`, `saturation` keep their existing ranges.
- Code in this plan is written for ruff and rustfmt as configured in the repo (`rustfmt.toml` max width 120); wrap any line `ruff check` or `cargo fmt` flags, without changing behaviour.
- Commit messages are plain, lower-case `feat: ...` / `docs: ...` lines with no co-author or generated-by line (the user's global rule).
- The pnpm migration is in the working tree and is **not** part of these commits: always `git add` explicit paths, never `-A`.

## Review Focus

- A source that never beat (`last_beat = -inf`) and is silent: no effect may raise, divide by zero or produce NaN; `beathue` shows its floor, `ambient` its base, `tempo` and `centroid` go dark. (Tasks 1, 2, 3, 5)
- Several beats between two frames: `tempo` must read two beats 0.5 s apart as 120 bpm, not one slow beat. (Task 2)
- Beat intervals outside 0.25..1.5 s (a 2 s gap, a double hit) must not move the tempo estimate. (Task 2)
- Music that returns gently after a lull must not make a later ordinary beat flash as a drop. (Task 4)
- A steady loud passage with beats must never flash. (Task 4)
- An invalid parameter on creation must be refused before the effect runs: `slow >= fast`, `step` outside 1..180, `period`/`width`/`flash`/`build` not positive. (Tasks 1-5)

---

### Task 1: `beathue` (Python and Rust) and the shared sensitivity check

**Files:**
- Modify: `src/chsmartbulb/music.py` (add `_check_sensitivity`, use it in `music_pulse`, add `music_beathue` after `music_pulse`)
- Modify: `src/chsmartbulb/catalog.py` (`_RANGES`, `_ENTRIES`)
- Modify: `crates/core/src/audio.rs` (add `music_beathue` after `music_pulse`, test in the `tests` module)
- Modify: `crates/core/src/catalog.rs` (`range()`, `CATALOG`, the `describe` test count)
- Test: `tests/test_catalog_and_music.py`

**Interfaces:**
- Produces (Python): `music._check_sensitivity(source: AudioSource, sensitivity: float) -> None`; `music.music_beathue(source, step=47.0, decay=5.0, floor=0.1, saturation=1.0, delay=0.0, sensitivity=DEFAULT_SENSITIVITY) -> Effect`.
- Produces (Rust): `audio::music_beathue(source: Arc<AudioSource>, step: f64, decay: f64, floor: f64, saturation: f64, delay: f64, sensitivity: f64) -> Result<Effect>`.
- Later tasks use `_check_sensitivity` and the same catalog edit points.

- [ ] **Step 0: Commit the spec and this plan**

```bash
git add docs/superpowers/specs/2026-10-07-music-effects-design.md docs/superpowers/plans/2026-10-07-music-effects.md
git commit -m "docs: design and plan for five more music effects"
```

- [ ] **Step 1: Write the failing Python tests**

Append to `tests/test_catalog_and_music.py`:

```python
def test_beathue_steps_the_hue_and_keeps_a_floor():
    source = FakeMusic()
    effect = catalog.create("beathue", {"step": 120, "floor": 0.2, "decay": 5}, audio=source)
    assert effect(0.0) == Color(r=51)  # never beat and silent: the floor, in the first hue (red)
    source.beat(at=10.0)
    source.now = 10.0
    assert effect(0.0) == Color(g=255)  # one step of 120 degrees is green, at full level
    source.now = 13.0
    assert effect(3.0) == Color(g=51)  # decayed to the floor
    source.beat(at=13.0)
    assert effect(3.0) == Color(b=255)  # two steps: blue


def test_beathue_refuses_what_does_not_fit():
    source = FakeMusic()
    bad = ({"step": 0}, {"step": 200}, {"decay": 0}, {"floor": 2}, {"saturation": -1}, {"sensitivity": 2}, {"delay": 5})
    for params in bad:
        with pytest.raises(ValueError):
            catalog.create("beathue", params, audio=source)
```

In `test_describe_is_plain_data_covering_every_effect`, change the expected `needs` dict to:

```python
    assert needs == {
        "music": "audio",
        "spectrum": "audio",
        "volume": "audio",
        "stereo": "audio",
        "beathue": "audio",
        "screen": "screen",
    }
```

- [ ] **Step 2: Run them to see them fail**

Run: `.venv/Scripts/python -m pytest tests/test_catalog_and_music.py -q -k "beathue or describe_is_plain"`
Expected: FAIL (`unknown effect 'beathue'` and the `needs` mismatch).

- [ ] **Step 3: Implement the Python side**

In `src/chsmartbulb/music.py`, directly after `_set_delay`, add:

```python
def _check_sensitivity(source: AudioSource, sensitivity: float) -> None:
    if not 0.0 <= sensitivity <= 1.0:
        raise ValueError("sensitivity must be within 0..1")
    source.sensitivity = sensitivity
```

In `music_pulse`, replace

```python
    _set_delay(source, delay)
    if not 0.0 <= sensitivity <= 1.0:
        raise ValueError("sensitivity must be within 0..1")
    source.sensitivity = sensitivity
```

with

```python
    _set_delay(source, delay)
    _check_sensitivity(source, sensitivity)
```

After `music_pulse`, add:

```python
def music_beathue(
    source: AudioSource,
    step: float = 47.0,
    decay: float = 5.0,
    floor: float = 0.1,
    saturation: float = 1.0,
    delay: float = 0.0,
    sensitivity: float = DEFAULT_SENSITIVITY,
) -> Effect:
    """Turn the colour by ``step`` degrees on every beat; flash and glow with the bass.

    Between beats the light falls to ``floor`` (0..1) of its brightness, never fully dark.
    """
    if not 1.0 <= step <= 180.0:
        raise ValueError("step must be within 1..180 degrees")
    if decay <= 0.0:
        raise ValueError("decay must be positive")
    if not 0.0 <= floor <= 1.0:
        raise ValueError("floor must be within 0..1")
    if not 0.0 <= saturation <= 1.0:
        raise ValueError("saturation must be within 0..1")
    _set_delay(source, delay)
    _check_sensitivity(source, sensitivity)

    def effect(t: float) -> Color:
        flash = math.exp(-decay * (source.clock() - source.last_beat))
        level = floor + (1.0 - floor) * max(flash, 0.6 * source.levels.bass)
        if level < _DARK:
            return OFF
        return Color.from_hsv(source.beats * step, saturation).scaled(level)

    return effect
```

In `src/chsmartbulb/catalog.py` add to `_RANGES`: `"step": (1.0, 180.0, 1.0),`. Add this entry to `_ENTRIES` right after the `stereo` entry:

```python
    EffectInfo(
        "beathue",
        "the colour turns on every beat, the brightness follows the bass",
        music.music_beathue,
        {
            "step": 47.0,
            "decay": 5.0,
            "floor": 0.1,
            "saturation": 1.0,
            "delay": 0.0,
            "sensitivity": music.DEFAULT_SENSITIVITY,
        },
        needs="audio",
    ),
```

- [ ] **Step 4: Run the Python tests**

Run: `.venv/Scripts/python -m pytest tests -q` and `.venv/Scripts/python -m ruff check`
Expected: all pass, ruff clean.

- [ ] **Step 5: Write the failing Rust test**

In the `tests` module of `crates/core/src/audio.rs`, add:

```rust
    #[test]
    fn beathue_steps_the_hue_and_keeps_a_floor() {
        let (clock, now) = manual_clock();
        let source = AudioSource::new(clock);
        let mut effect = music_beathue(source.clone(), 120.0, 5.0, 0.2, 1.0, 0.0, 0.5).unwrap();
        assert_eq!(effect(0.0), Color::rgb(51, 0, 0)); // never beat and silent: the floor, in red
        set(&now, 10.0);
        source.publish(Levels::default(), 10.0); // a beat, the bass silent
        assert_eq!(effect(0.0), Color::rgb(0, 255, 0));
        set(&now, 13.0);
        assert_eq!(effect(3.0), Color::rgb(0, 51, 0));
        source.publish(Levels::default(), 10.0);
        assert_eq!(effect(3.0), Color::rgb(0, 0, 255));
    }

    #[test]
    fn beathue_refuses_what_does_not_fit() {
        let (clock, _) = manual_clock();
        let source = AudioSource::new(clock);
        for (step, decay, floor, saturation, delay, sensitivity) in [
            (0.0, 5.0, 0.1, 1.0, 0.0, 0.5),
            (200.0, 5.0, 0.1, 1.0, 0.0, 0.5),
            (47.0, 0.0, 0.1, 1.0, 0.0, 0.5),
            (47.0, 5.0, 2.0, 1.0, 0.0, 0.5),
            (47.0, 5.0, 0.1, -1.0, 0.0, 0.5),
            (47.0, 5.0, 0.1, 1.0, 5.0, 0.5),
            (47.0, 5.0, 0.1, 1.0, 0.0, 2.0),
        ] {
            assert!(music_beathue(source.clone(), step, decay, floor, saturation, delay, sensitivity).is_err());
        }
    }
```

In `crates/core/src/catalog.rs`, the `describe_matches_the_python_shape` test: change `assert_eq!(all.len(), 13);` to `14` and add after the `music` assertions:

```rust
        let beathue = all.iter().find(|e| e["name"] == "beathue").unwrap();
        assert_eq!(beathue["needs"], "audio");
        assert_eq!(beathue["schema"]["step"], json!({"type": "number", "min": 1.0, "max": 180.0, "step": 1.0}));
```

- [ ] **Step 6: Run to see them fail**

Run: `cargo test -p chsmartbulb-core beathue`
Expected: FAIL to compile (`music_beathue` not found).

- [ ] **Step 7: Implement the Rust side**

In `crates/core/src/audio.rs`, after `music_pulse`:

```rust
/// Turn the colour by `step` degrees on every beat; flash and glow with the bass.
///
/// Between beats the light falls to `floor` of its brightness, never fully dark.
pub fn music_beathue(
    source: Arc<AudioSource>,
    step: f64,
    decay: f64,
    floor: f64,
    saturation: f64,
    delay: f64,
    sensitivity: f64,
) -> Result<Effect> {
    if !(1.0..=180.0).contains(&step) {
        return Err(invalid("step must be within 1..180 degrees"));
    }
    if decay <= 0.0 {
        return Err(invalid("decay must be positive"));
    }
    if !(0.0..=1.0).contains(&floor) {
        return Err(invalid("floor must be within 0..1"));
    }
    if !(0.0..=1.0).contains(&saturation) {
        return Err(invalid("saturation must be within 0..1"));
    }
    source.set_delay(delay)?;
    source.set_sensitivity(sensitivity)?;
    Ok(Box::new(move |_t| {
        let heard = source.heard();
        let flash = (-decay * (heard.now - heard.last_beat)).exp();
        let level = floor + (1.0 - floor) * flash.max(0.6 * heard.levels.bass);
        if level < DARK {
            return OFF;
        }
        Color::from_hsv(heard.beats as f64 * step, saturation, 1.0).scaled(level)
    }))
}
```

In `crates/core/src/catalog.rs`, add to `range()` before the `_ =>` arm: `"step" => (1.0, 180.0, 1.0),`. Add to `CATALOG` right after the `stereo` entry:

```rust
    EffectInfo {
        name: "beathue",
        summary: "the colour turns on every beat, the brightness follows the bass",
        needs: Some(Needs::Audio),
        defaults: || {
            vec![
                ("step", Param::Number(47.0)),
                ("decay", Param::Number(5.0)),
                ("floor", Param::Number(0.1)),
                ("saturation", Param::Number(1.0)),
                ("delay", Param::Number(0.0)),
                ("sensitivity", Param::Number(DEFAULT_SENSITIVITY)),
            ]
        },
        build: |p, s| {
            audio::music_beathue(
                audio(s, "beathue")?,
                n(p, "step"),
                n(p, "decay"),
                n(p, "floor"),
                n(p, "saturation"),
                n(p, "delay"),
                n(p, "sensitivity"),
            )
        },
        ranges: &[],
    },
```

- [ ] **Step 8: Run the Rust checks**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all pass.

- [ ] **Step 9: Commit**

```bash
git add src/chsmartbulb/music.py src/chsmartbulb/catalog.py tests/test_catalog_and_music.py crates/core/src/audio.rs crates/core/src/catalog.rs
git commit -m "feat: beathue turns the colour on every beat and keeps a floor of light"
```

---

### Task 2: `tempo`

**Files:**
- Modify: `src/chsmartbulb/music.py`, `src/chsmartbulb/catalog.py`, `crates/core/src/audio.rs`, `crates/core/src/catalog.rs`
- Test: `tests/test_catalog_and_music.py`

**Interfaces:**
- Consumes: `_set_delay`, `_check_sensitivity`, `_loudness`, `_DARK` (Python); `source.set_delay`, `source.set_sensitivity`, `Levels::loudness`, `DARK` (Rust).
- Produces: `music.music_tempo(source, slow=80.0, fast=160.0, smoothing=2.0, delay=0.0, sensitivity=DEFAULT_SENSITIVITY) -> Effect`; `audio::music_tempo(source: Arc<AudioSource>, slow: f64, fast: f64, smoothing: f64, delay: f64, sensitivity: f64) -> Result<Effect>`.

- [ ] **Step 1: Write the failing Python tests**

```python
def test_tempo_colours_by_how_fast_the_beats_come():
    source = FakeMusic()
    source.levels = music.Levels(bass=1.0)
    params = {"slow": 80, "fast": 120, "smoothing": 0}
    tempo = catalog.create("tempo", params, audio=source)
    source.beat(at=10.0)
    source.now = 10.0
    tempo(0.0)  # the first beat gives no interval yet
    source.beat(at=10.5)
    source.now = 10.5
    assert tempo(0.5) == Color(g=160, b=255)  # 120 bpm is the fast end
    source.beat(at=10.6)  # 0.1 s apart is no tempo
    assert tempo(0.6) == Color(g=160, b=255)
    source.beat(at=12.6)  # neither is 2 s
    assert tempo(2.6) == Color(g=160, b=255)
    source.now = 30.0
    assert tempo(20.0) == Color(g=160, b=255)  # without beats the estimate holds

    slow = catalog.create("tempo", params, audio=source)
    source.beat(at=40.0)
    slow(0.0)
    source.beat(at=41.0)
    assert slow(1.0) == Color(r=255, g=60)  # 60 bpm is below slow


def test_tempo_reads_beats_that_arrive_between_two_frames():
    source = FakeMusic()
    source.levels = music.Levels(bass=1.0)
    tempo = catalog.create("tempo", {"slow": 80, "fast": 120, "smoothing": 0}, audio=source)
    source.beat(at=10.0)
    tempo(0.0)
    source.beat(at=10.5)
    source.beat(at=11.0)  # two beats before the next frame: 0.5 s apart, not one 1 s beat
    assert tempo(1.0) == Color(g=160, b=255)


def test_tempo_is_dark_in_silence_and_refuses_what_does_not_fit():
    source = FakeMusic()
    tempo = catalog.create("tempo", {}, audio=source)
    assert tempo(0.0) == Color()  # never beat and silent
    for params in ({"slow": 120, "fast": 120}, {"slow": 160, "fast": 80}, {"smoothing": -1}, {"sensitivity": 2}):
        with pytest.raises(ValueError):
            catalog.create("tempo", params, audio=source)
```

Add `"tempo": "audio",` after `"beathue": "audio",` in the `needs` dict of `test_describe_is_plain_data_covering_every_effect`.

- [ ] **Step 2: Run to see them fail**

Run: `.venv/Scripts/python -m pytest tests/test_catalog_and_music.py -q -k tempo`
Expected: FAIL (`unknown effect 'tempo'`).

- [ ] **Step 3: Implement the Python side**

In `music.py`, near the other module constants (after `_ONSET_CAP`), add:

```python
_TEMPO_SLOW = Color(r=255, g=60)
_TEMPO_FAST = Color(g=160, b=255)
_TEMPO_INTERVALS = (0.25, 1.5)  # seconds between beats that count as a tempo, 240 to 40 bpm
_TEMPO_BLEND = 0.3  # share of a new interval in the estimate
_TEMPO_RELEASE = 3.0  # brightness falls this much per second
```

After `music_beathue`, add:

```python
def music_tempo(
    source: AudioSource,
    slow: float = 80.0,
    fast: float = 160.0,
    smoothing: float = 2.0,
    delay: float = 0.0,
    sensitivity: float = DEFAULT_SENSITIVITY,
) -> Effect:
    """Warm for slow music, cool for fast; brightness follows the loudness.

    The tempo is estimated from the gaps between beats. ``slow`` and ``fast`` (bpm) are the
    tempos shown as the warm and the cool end, ``smoothing`` the seconds the colour takes to settle.
    """
    if slow >= fast:
        raise ValueError("slow must be below fast")
    if smoothing < 0.0:
        raise ValueError("smoothing must not be negative")
    _set_delay(source, delay)
    _check_sensitivity(source, sensitivity)
    estimate: float | None = None  # seconds between beats
    previous: float | None = None
    seen = source.beats
    position = 0.5
    shown = 0.0
    last = 0.0

    def effect(t: float) -> Color:
        nonlocal estimate, previous, seen, position, shown, last
        step = max(0.0, t - last)
        last = t
        if source.beats != seen:
            risen = source.beats - seen
            seen = source.beats
            if risen > 0 and previous is not None:
                interval = (source.last_beat - previous) / risen
                if _TEMPO_INTERVALS[0] <= interval <= _TEMPO_INTERVALS[1]:
                    estimate = interval if estimate is None else (1.0 - _TEMPO_BLEND) * estimate + _TEMPO_BLEND * interval
            previous = source.last_beat
        target = 0.5 if estimate is None else min(1.0, max(0.0, (60.0 / estimate - slow) / (fast - slow)))
        if smoothing > 0.0:
            position += (target - position) * (1.0 - math.exp(-step / smoothing))
        else:
            position = target
        shown = max(_loudness(source.levels), shown - _TEMPO_RELEASE * step)
        level = shown * shown
        if level < _DARK:
            return OFF
        return _TEMPO_SLOW.mix(_TEMPO_FAST, position).scaled(level)

    return effect
```

In `catalog.py` add to `_RANGES`: `"slow": (40.0, 240.0, 1.0),` and `"fast": (40.0, 240.0, 1.0),`. Add after the `beathue` entry:

```python
    EffectInfo(
        "tempo",
        "warm for slow music, cool for fast, by the gaps between beats",
        music.music_tempo,
        {"slow": 80.0, "fast": 160.0, "smoothing": 2.0, "delay": 0.0, "sensitivity": music.DEFAULT_SENSITIVITY},
        needs="audio",
    ),
```

- [ ] **Step 4: Run the Python tests**

Run: `.venv/Scripts/python -m pytest tests -q` and `.venv/Scripts/python -m ruff check`
Expected: all pass. If ruff reports a line over the limit, wrap the `estimate = ...` line.

- [ ] **Step 5: Write the failing Rust tests**

In the `tests` module of `audio.rs`:

```rust
    #[test]
    fn tempo_colours_by_how_fast_the_beats_come() {
        let (clock, now) = manual_clock();
        let source = AudioSource::new(clock);
        let loud = Levels { bass: 1.0, ..Levels::default() };
        let mut effect = music_tempo(source.clone(), 80.0, 120.0, 0.0, 0.0, 0.5).unwrap();
        set(&now, 10.0);
        source.publish(loud, 10.0);
        effect(0.0); // the first beat gives no interval yet
        set(&now, 10.5);
        source.publish(loud, 10.0);
        assert_eq!(effect(0.5), Color::rgb(0, 160, 255)); // 120 bpm is the fast end
        set(&now, 12.5);
        source.publish(loud, 10.0); // 2 s apart is no tempo
        assert_eq!(effect(2.5), Color::rgb(0, 160, 255));
        set(&now, 30.0);
        assert_eq!(effect(20.0), Color::rgb(0, 160, 255)); // without beats the estimate holds

        let mut slow = music_tempo(source.clone(), 80.0, 120.0, 0.0, 0.0, 0.5).unwrap();
        set(&now, 40.0);
        source.publish(loud, 10.0);
        slow(0.0);
        set(&now, 41.0);
        source.publish(loud, 10.0);
        assert_eq!(slow(1.0), Color::rgb(255, 60, 0)); // 60 bpm is below slow
    }

    #[test]
    fn tempo_reads_beats_that_arrive_between_two_frames() {
        let (clock, now) = manual_clock();
        let source = AudioSource::new(clock);
        let loud = Levels { bass: 1.0, ..Levels::default() };
        let mut effect = music_tempo(source.clone(), 80.0, 120.0, 0.0, 0.0, 0.5).unwrap();
        set(&now, 10.0);
        source.publish(loud, 10.0);
        effect(0.0);
        set(&now, 10.5);
        source.publish(loud, 10.0);
        set(&now, 11.0);
        source.publish(loud, 10.0); // two beats before the next frame, 0.5 s apart
        assert_eq!(effect(1.0), Color::rgb(0, 160, 255));
    }

    #[test]
    fn tempo_is_dark_in_silence_and_refuses_what_does_not_fit() {
        let (clock, _) = manual_clock();
        let source = AudioSource::new(clock);
        let mut effect = music_tempo(source.clone(), 80.0, 160.0, 2.0, 0.0, 0.5).unwrap();
        assert_eq!(effect(0.0), OFF);
        for (slow, fast, smoothing, sensitivity) in
            [(120.0, 120.0, 2.0, 0.5), (160.0, 80.0, 2.0, 0.5), (80.0, 160.0, -1.0, 0.5), (80.0, 160.0, 2.0, 2.0)]
        {
            assert!(music_tempo(source.clone(), slow, fast, smoothing, 0.0, sensitivity).is_err());
        }
    }
```

In `catalog.rs`, change the `describe` test count `14` to `15`.

- [ ] **Step 6: Run to see them fail**

Run: `cargo test -p chsmartbulb-core tempo`
Expected: FAIL to compile (`music_tempo` not found).

- [ ] **Step 7: Implement the Rust side**

In `audio.rs` constants (after `DARK`):

```rust
const TEMPO_SLOW: Color = Color::rgb(255, 60, 0);
const TEMPO_FAST: Color = Color::rgb(0, 160, 255);
/// Seconds between beats that count as a tempo: 240 to 40 bpm.
const TEMPO_INTERVALS: (f64, f64) = (0.25, 1.5);
/// Share of a new interval in the estimate.
const TEMPO_BLEND: f64 = 0.3;
/// Brightness falls this much per second.
const TEMPO_RELEASE: f64 = 3.0;
```

After `music_beathue`:

```rust
/// Warm for slow music, cool for fast; brightness follows the loudness.
///
/// The tempo is estimated from the gaps between beats. `slow` and `fast` (bpm) are the
/// tempos shown as the warm and the cool end, `smoothing` the seconds the colour takes to settle.
pub fn music_tempo(
    source: Arc<AudioSource>,
    slow: f64,
    fast: f64,
    smoothing: f64,
    delay: f64,
    sensitivity: f64,
) -> Result<Effect> {
    if slow >= fast {
        return Err(invalid("slow must be below fast"));
    }
    if smoothing < 0.0 {
        return Err(invalid("smoothing must not be negative"));
    }
    source.set_delay(delay)?;
    source.set_sensitivity(sensitivity)?;
    let mut estimate: Option<f64> = None; // seconds between beats
    let mut previous: Option<f64> = None;
    let mut seen = source.heard().beats;
    let mut position = 0.5f64;
    let mut shown = 0.0f64;
    let mut last = 0.0;
    Ok(Box::new(move |t| {
        let step = (t - last).max(0.0);
        last = t;
        let heard = source.heard();
        if heard.beats != seen {
            if heard.beats > seen {
                if let Some(before) = previous {
                    let interval = (heard.last_beat - before) / (heard.beats - seen) as f64;
                    if (TEMPO_INTERVALS.0..=TEMPO_INTERVALS.1).contains(&interval) {
                        estimate = Some(match estimate {
                            Some(old) => (1.0 - TEMPO_BLEND) * old + TEMPO_BLEND * interval,
                            None => interval,
                        });
                    }
                }
            }
            seen = heard.beats;
            previous = Some(heard.last_beat);
        }
        let target = match estimate {
            Some(interval) => ((60.0 / interval - slow) / (fast - slow)).clamp(0.0, 1.0),
            None => 0.5,
        };
        if smoothing > 0.0 {
            position += (target - position) * (1.0 - (-step / smoothing).exp());
        } else {
            position = target;
        }
        shown = heard.levels.loudness().max(shown - TEMPO_RELEASE * step);
        let level = shown * shown;
        if level < DARK {
            return OFF;
        }
        TEMPO_SLOW.mix(&TEMPO_FAST, position).scaled(level)
    }))
}
```

In `catalog.rs`: add to `range()` before `_ =>`: `"slow" | "fast" => (40.0, 240.0, 1.0),`. Add after the `beathue` entry:

```rust
    EffectInfo {
        name: "tempo",
        summary: "warm for slow music, cool for fast, by the gaps between beats",
        needs: Some(Needs::Audio),
        defaults: || {
            vec![
                ("slow", Param::Number(80.0)),
                ("fast", Param::Number(160.0)),
                ("smoothing", Param::Number(2.0)),
                ("delay", Param::Number(0.0)),
                ("sensitivity", Param::Number(DEFAULT_SENSITIVITY)),
            ]
        },
        build: |p, s| {
            audio::music_tempo(
                audio(s, "tempo")?,
                n(p, "slow"),
                n(p, "fast"),
                n(p, "smoothing"),
                n(p, "delay"),
                n(p, "sensitivity"),
            )
        },
        ranges: &[],
    },
```

- [ ] **Step 8: Run the Rust checks**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all pass.

- [ ] **Step 9: Commit**

```bash
git add src/chsmartbulb/music.py src/chsmartbulb/catalog.py tests/test_catalog_and_music.py crates/core/src/audio.rs crates/core/src/catalog.rs
git commit -m "feat: tempo colours the light by how fast the beats come"
```

---

### Task 3: `centroid`

**Files:**
- Modify: `src/chsmartbulb/music.py`, `src/chsmartbulb/catalog.py`, `crates/core/src/audio.rs`, `crates/core/src/catalog.rs`
- Test: `tests/test_catalog_and_music.py`

**Interfaces:**
- Consumes: `_set_delay`, `_loudness`, `_DARK`, `_PAN_SMOOTHING`, `RED`, `BLUE` (Python); `PAN_SMOOTHING`, `DARK`, `RED`, `BLUE` (Rust).
- Produces: `music.music_centroid(source, low=RED, high=BLUE, width=2.0, release=3.0, delay=0.0) -> Effect`; `audio::music_centroid(source: Arc<AudioSource>, low: Color, high: Color, width: f64, release: f64, delay: f64) -> Result<Effect>`.

- [ ] **Step 1: Write the failing Python tests**

```python
def test_centroid_blends_by_where_the_weight_lies():
    source = FakeMusic()
    centroid = catalog.create("centroid", {"low": "ff0000", "high": "0000ff", "width": 1, "release": 2.0}, audio=source)
    assert centroid(0.0) == Color()  # silent: dark, and no division by zero
    source.levels = music.Levels(bass=1.0)
    assert centroid(1.0) == Color(r=255)  # all bass is the low colour
    source.levels = music.Levels(treble=1.0)
    assert centroid(1.05) == Color(r=183, b=72)  # on its way to the high colour
    source.levels = music.Levels(mid=1.0)
    even = centroid(101.0)  # out of the dark a sound shows where it is at once
    assert 100 < even.r < 160
    assert 100 < even.b < 160
    source.levels = music.Levels()
    quiet = centroid(101.2)
    assert quiet.r > 0
    assert quiet.b > 0  # silence keeps the position


def test_centroid_refuses_what_does_not_fit():
    source = FakeMusic()
    with pytest.raises(ValueError, match="width"):
        catalog.create("centroid", {"width": 0}, audio=source)
    with pytest.raises(ValueError, match="delay"):
        catalog.create("centroid", {"delay": 5}, audio=source)
```

Add `"centroid": "audio",` after `"tempo": "audio",` in the `needs` dict.

- [ ] **Step 2: Run to see them fail**

Run: `.venv/Scripts/python -m pytest tests/test_catalog_and_music.py -q -k centroid`
Expected: FAIL (`unknown effect 'centroid'`).

- [ ] **Step 3: Implement the Python side**

In `music.py`, after `music_tempo`:

```python
def music_centroid(
    source: AudioSource,
    low: Color = RED,
    high: Color = BLUE,
    width: float = 2.0,
    release: float = 3.0,
    delay: float = 0.0,
) -> Effect:
    """Blend between two colours by where the sound's weight lies between bass and treble.

    Bass-heavy sound shows ``low``, bright sound ``high``. The weight of real music seldom passes
    the middle, so ``width`` stretches it. Brightness follows the loudness.
    """
    if width <= 0.0:
        raise ValueError("width must be positive")
    _set_delay(source, delay)
    shown = 0.0
    position = 0.0
    last = 0.0

    def effect(t: float) -> Color:
        nonlocal shown, position, last
        step = max(0.0, t - last)
        last = t
        levels = source.levels
        total = levels.bass + levels.mid + levels.treble
        faded = max(0.0, shown - release * step)
        if total > 0.0:  # silence says nothing about the weight, keep the last one
            target = min(1.0, max(0.0, width * (0.5 * levels.mid + levels.treble) / total))
            if faded * faded < _DARK:
                position = target
            else:
                position += (target - position) * (1.0 - math.exp(-step / _PAN_SMOOTHING))
        shown = max(_loudness(levels), faded)
        level = shown * shown
        return low.mix(high, position).scaled(level) if level >= _DARK else OFF

    return effect
```

In `catalog.py`, after the `tempo` entry:

```python
    EffectInfo(
        "centroid",
        "blend two colours by whether the sound is bass-heavy or bright",
        music.music_centroid,
        {"low": RED, "high": BLUE, "width": 2.0, "release": 3.0, "delay": 0.0},
        needs="audio",
    ),
```

(`RED` and `BLUE` are already imported in `catalog.py`; check the import line and add what is missing.)

- [ ] **Step 4: Run the Python tests**

Run: `.venv/Scripts/python -m pytest tests -q` and `.venv/Scripts/python -m ruff check`
Expected: all pass.

- [ ] **Step 5: Write the failing Rust tests**

```rust
    #[test]
    fn centroid_blends_by_where_the_weight_lies() {
        let (clock, _) = manual_clock();
        let source = AudioSource::new(clock);
        let mut effect = music_centroid(source.clone(), RED, BLUE, 1.0, 2.0, 0.0).unwrap();
        assert_eq!(effect(0.0), OFF); // silent: dark, and no division by zero
        source.publish(Levels { bass: 1.0, ..Levels::default() }, 0.0);
        assert_eq!(effect(1.0), Color::rgb(255, 0, 0));
        source.publish(Levels { treble: 1.0, ..Levels::default() }, 0.0);
        assert_eq!(effect(1.05), Color::rgb(183, 0, 72));
        source.publish(Levels { mid: 1.0, ..Levels::default() }, 0.0);
        let even = effect(101.0); // out of the dark a sound shows where it is at once
        assert!(100 < even.r && even.r < 160);
        assert!(100 < even.b && even.b < 160);
        source.publish(Levels::default(), 0.0);
        let quiet = effect(101.2);
        assert!(quiet.r > 0 && quiet.b > 0); // silence keeps the position
    }

    #[test]
    fn centroid_refuses_what_does_not_fit() {
        let (clock, _) = manual_clock();
        let source = AudioSource::new(clock);
        assert!(music_centroid(source.clone(), RED, BLUE, 0.0, 3.0, 0.0).is_err());
        assert!(music_centroid(source.clone(), RED, BLUE, 2.0, 3.0, 5.0).is_err());
    }
```

In `catalog.rs` change the `describe` count `15` to `16`.

- [ ] **Step 6: Run to see them fail**

Run: `cargo test -p chsmartbulb-core centroid`
Expected: FAIL to compile.

- [ ] **Step 7: Implement the Rust side**

In `audio.rs`, after `music_tempo`:

```rust
/// Blend between two colours by where the sound's weight lies between bass and treble.
///
/// The weight of real music seldom passes the middle, so `width` stretches it.
/// Brightness follows the loudness.
pub fn music_centroid(
    source: Arc<AudioSource>,
    low: Color,
    high: Color,
    width: f64,
    release: f64,
    delay: f64,
) -> Result<Effect> {
    if width <= 0.0 {
        return Err(invalid("width must be positive"));
    }
    source.set_delay(delay)?;
    let mut shown = 0.0f64;
    let mut position = 0.0f64;
    let mut last = 0.0;
    Ok(Box::new(move |t| {
        let step = (t - last).max(0.0);
        last = t;
        let levels = source.heard().levels;
        let total = levels.bass + levels.mid + levels.treble;
        let faded = (shown - release * step).max(0.0);
        if total > 0.0 {
            // silence says nothing about the weight, keep the last one
            let target = (width * (0.5 * levels.mid + levels.treble) / total).clamp(0.0, 1.0);
            if faded * faded < DARK {
                position = target;
            } else {
                position += (target - position) * (1.0 - (-step / PAN_SMOOTHING).exp());
            }
        }
        shown = levels.loudness().max(faded);
        let level = shown * shown;
        if level >= DARK {
            low.mix(&high, position).scaled(level)
        } else {
            OFF
        }
    }))
}
```

In `catalog.rs`, add after the `tempo` entry (`BLUE` and `RED` are already imported there):

```rust
    EffectInfo {
        name: "centroid",
        summary: "blend two colours by whether the sound is bass-heavy or bright",
        needs: Some(Needs::Audio),
        defaults: || {
            vec![
                ("low", Param::Color(Some(RED))),
                ("high", Param::Color(Some(BLUE))),
                ("width", Param::Number(2.0)),
                ("release", Param::Number(3.0)),
                ("delay", Param::Number(0.0)),
            ]
        },
        build: |p, s| {
            audio::music_centroid(
                audio(s, "centroid")?,
                colour(p, "low"),
                colour(p, "high"),
                n(p, "width"),
                n(p, "release"),
                n(p, "delay"),
            )
        },
        ranges: &[],
    },
```

- [ ] **Step 8: Run the Rust checks**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all pass. If `Color::rgb(183, 0, 72)` differs by one, recompute: `1 - exp(-0.05/0.15)` is 0.28347, so red is `round(255 * 0.71653) = 183` and blue `round(255 * 0.28347) = 72`; a different result means the arithmetic diverged from the Python, which is the bug to fix.

- [ ] **Step 9: Commit**

```bash
git add src/chsmartbulb/music.py src/chsmartbulb/catalog.py tests/test_catalog_and_music.py crates/core/src/audio.rs crates/core/src/catalog.rs
git commit -m "feat: centroid blends two colours by where the sound's weight lies"
```

---

### Task 4: `drop`

**Files:**
- Modify: `src/chsmartbulb/music.py`, `src/chsmartbulb/catalog.py`, `crates/core/src/audio.rs`, `crates/core/src/catalog.rs`
- Test: `tests/test_catalog_and_music.py`

**Interfaces:**
- Consumes: `_set_delay`, `_check_sensitivity`, `_loudness`, `OFF`, `WHITE` (Python; add `WHITE` to the `.color` import); `OFF`, `WHITE` (Rust; add `WHITE` to the `crate::color` import).
- Produces: `music.music_drop(source, color=WHITE, flash=0.4, build=3.0, delay=0.0, sensitivity=DEFAULT_SENSITIVITY) -> Effect`; `audio::music_drop(source: Arc<AudioSource>, color: Color, flash: f64, build: f64, delay: f64, sensitivity: f64) -> Result<Effect>`.

- [ ] **Step 1: Write the failing Python tests**

```python
def advance(effect, t, seconds, step=0.05):
    """Run an effect for ``seconds`` in frames of ``step``; return the time and the last frame."""
    frame = None
    for _ in range(round(seconds / step)):
        t += step
        frame = effect(t)
    return t, frame


def test_drop_flashes_when_the_music_comes_back_after_a_lull():
    source = FakeMusic()
    effect = catalog.create("drop", {"color": "00ff00", "flash": 0.4, "build": 3.0}, audio=source)
    source.levels = music.Levels(bass=1.0)
    t, _ = advance(effect, 0.0, 20)  # steady loud music
    source.beat(at=t)
    t += 0.05
    assert effect(t).g < 255  # a beat in steady music is no drop
    source.levels = music.Levels()
    t, quiet = advance(effect, t, 8)
    assert 0 < quiet.g < 100  # a dim glow in the lull
    source.levels = music.Levels(bass=1.0)
    source.beat(at=t + 0.05)
    t += 0.05
    began = t
    assert effect(t) == Color(g=255)  # the drop: flash on
    assert effect(began + 0.07) == Color()  # and off, at 8 Hz
    assert effect(began + 0.5).g < 255  # after the flash it is the music's glow again


def test_drop_ignores_music_that_comes_back_gently():
    source = FakeMusic()
    effect = catalog.create("drop", {"color": "00ff00"}, audio=source)
    source.levels = music.Levels(bass=1.0)
    t, _ = advance(effect, 0.0, 20)
    source.levels = music.Levels()
    t, _ = advance(effect, t, 8)  # a lull
    source.levels = music.Levels(bass=0.3)
    t, _ = advance(effect, t, 30)  # back, but quietly: no beat, no flash
    source.levels = music.Levels(bass=1.0)
    source.beat(at=t + 0.05)
    t += 0.05
    assert effect(t).g < 255  # an ordinary beat much later is not a drop


def test_drop_refuses_what_does_not_fit():
    source = FakeMusic()
    for params in ({"flash": 0}, {"build": 0}, {"sensitivity": 2}, {"delay": 5}):
        with pytest.raises(ValueError):
            catalog.create("drop", params, audio=source)
```

Add `"drop": "audio",` after `"centroid": "audio",` in the `needs` dict.

- [ ] **Step 2: Run to see them fail**

Run: `.venv/Scripts/python -m pytest tests/test_catalog_and_music.py -q -k drop`
Expected: FAIL (`unknown effect 'drop'`).

- [ ] **Step 3: Implement the Python side**

In `music.py` add `WHITE` to `from .color import BLUE, OFF, RED, WHITE, Color`. Constants after the tempo ones:

```python
_DROP_SLOW = 8.0  # seconds; the long average the lull is measured against
_LULL_RATIO = 0.35  # a lull: the short average under this share of the long one
_LULL_FLOOR = 0.05  # and the long one above this, so there was something to fall from
_LULL_HOLD = 0.5  # seconds a lull must last
_DROP_ENERGY = 0.6  # a drop's beat must be this loud
_FLASH_HZ = 8.0
```

After `music_centroid`:

```python
def music_drop(
    source: AudioSource,
    color: Color = WHITE,
    flash: float = 0.4,
    build: float = 3.0,
    delay: float = 0.0,
    sensitivity: float = DEFAULT_SENSITIVITY,
) -> Effect:
    """Open up as the music builds and flash when it comes back in after a lull.

    ``build`` is the seconds the light takes to follow the energy, ``flash`` how long the flash lasts.
    The thresholds are tuned on synthetic music, not on real tracks.
    """
    if flash <= 0.0:
        raise ValueError("flash must be positive")
    if build <= 0.0:
        raise ValueError("build must be positive")
    _set_delay(source, delay)
    _check_sensitivity(source, sensitivity)
    fast = 0.0
    slow = 0.0
    lull = 0.0
    lulled = False
    seen = source.beats
    began = -math.inf
    last = 0.0

    def effect(t: float) -> Color:
        nonlocal fast, slow, lull, lulled, seen, began, last
        step = max(0.0, t - last)
        last = t
        energy = _loudness(source.levels)
        fast += (energy - fast) * (1.0 - math.exp(-step / build))
        slow += (energy - slow) * (1.0 - math.exp(-step / _DROP_SLOW))
        if slow > _LULL_FLOOR and fast < _LULL_RATIO * slow:
            lull += step
            if lull >= _LULL_HOLD:
                lulled = True
        else:
            lull = 0.0
            if fast >= slow:
                lulled = False  # the music came back gently: a later beat is not a drop
        arrived = source.beats != seen
        seen = source.beats
        if arrived and lulled and energy >= _DROP_ENERGY:
            lulled = False
            began = t
        if t - began < flash:
            return color if ((t - began) * _FLASH_HZ) % 1.0 < 0.5 else OFF
        return color.scaled(0.15 + 0.6 * fast)

    return effect
```

In `catalog.py`, add to `_RANGES`: `"flash": (0.1, 2.0, 0.05),` and `"build": (0.5, 10.0, 0.5),`. Make sure `WHITE` is imported from `.color`. After the `centroid` entry:

```python
    EffectInfo(
        "drop",
        "opens up as the music builds, flashes when it drops back in",
        music.music_drop,
        {"color": WHITE, "flash": 0.4, "build": 3.0, "delay": 0.0, "sensitivity": music.DEFAULT_SENSITIVITY},
        needs="audio",
    ),
```

- [ ] **Step 4: Run the Python tests**

Run: `.venv/Scripts/python -m pytest tests -q` and `.venv/Scripts/python -m ruff check`
Expected: all pass. The numbers in the tests (a 20 s loud stretch, an 8 s lull, 30 s of quiet return) come from a hand calculation of the two averages; if a scenario fails, print `fast` and `slow` per frame to see which threshold the scenario misses before changing a constant, and keep the constants identical in the Rust copy.

- [ ] **Step 5: Write the failing Rust tests**

In the `tests` module add a helper and the tests:

```rust
    fn advance(effect: &mut Effect, t: &mut f64, seconds: f64) -> Color {
        let mut frame = OFF;
        for _ in 0..(seconds / 0.05).round() as usize {
            *t += 0.05;
            frame = effect(*t);
        }
        frame
    }

    #[test]
    fn drop_flashes_when_the_music_comes_back_after_a_lull() {
        let (clock, now) = manual_clock();
        let source = AudioSource::new(clock);
        let loud = Levels { bass: 1.0, ..Levels::default() };
        let green = Color::rgb(0, 255, 0);
        let mut effect = music_drop(source.clone(), green, 0.4, 3.0, 0.0, 0.5).unwrap();
        let mut t = 0.0;
        source.publish(loud, 0.0);
        advance(&mut effect, &mut t, 20.0); // steady loud music
        set(&now, t);
        source.publish(loud, 10.0); // a beat
        t += 0.05;
        assert!(effect(t).g < 255); // no drop in steady music
        set(&now, t);
        source.publish(Levels::default(), 0.0);
        let quiet = advance(&mut effect, &mut t, 8.0);
        assert!(0 < quiet.g && quiet.g < 100); // a dim glow in the lull
        t += 0.05;
        set(&now, t);
        source.publish(loud, 10.0);
        let began = t;
        assert_eq!(effect(t), green); // the drop: flash on
        assert_eq!(effect(began + 0.07), OFF); // and off, at 8 Hz
        assert!(effect(began + 0.5).g < 255);
    }

    #[test]
    fn drop_ignores_music_that_comes_back_gently() {
        let (clock, now) = manual_clock();
        let source = AudioSource::new(clock);
        let loud = Levels { bass: 1.0, ..Levels::default() };
        let mut effect = music_drop(source.clone(), Color::rgb(0, 255, 0), 0.4, 3.0, 0.0, 0.5).unwrap();
        let mut t = 0.0;
        source.publish(loud, 0.0);
        advance(&mut effect, &mut t, 20.0);
        source.publish(Levels::default(), 0.0);
        advance(&mut effect, &mut t, 8.0); // a lull
        source.publish(Levels { bass: 0.3, ..Levels::default() }, 0.0);
        advance(&mut effect, &mut t, 30.0); // back, but quietly
        t += 0.05;
        set(&now, t);
        source.publish(loud, 10.0);
        assert!(effect(t).g < 255); // an ordinary beat much later is not a drop
    }

    #[test]
    fn drop_refuses_what_does_not_fit() {
        let (clock, _) = manual_clock();
        let source = AudioSource::new(clock);
        for (flash, build, delay, sensitivity) in
            [(0.0, 3.0, 0.0, 0.5), (0.4, 0.0, 0.0, 0.5), (0.4, 3.0, 5.0, 0.5), (0.4, 3.0, 0.0, 2.0)]
        {
            assert!(music_drop(source.clone(), WHITE, flash, build, delay, sensitivity).is_err());
        }
    }
```

In the `tests` module's `use super::*;`, `WHITE` needs to be imported at the top of `audio.rs`: change `use crate::color::{Color, BLUE, OFF, RED};` to `use crate::color::{Color, BLUE, OFF, RED, WHITE};` (Step 7 does it; the tests fail to compile until then). In `catalog.rs` change the `describe` count `16` to `17`.

- [ ] **Step 6: Run to see them fail**

Run: `cargo test -p chsmartbulb-core drop`
Expected: FAIL to compile.

- [ ] **Step 7: Implement the Rust side**

In `audio.rs` constants:

```rust
/// Seconds; the long average the lull is measured against.
const DROP_SLOW: f64 = 8.0;
/// A lull: the short average under this share of the long one.
const LULL_RATIO: f64 = 0.35;
/// And the long one above this, so there was something to fall from.
const LULL_FLOOR: f64 = 0.05;
/// Seconds a lull must last.
const LULL_HOLD: f64 = 0.5;
/// A drop's beat must be this loud.
const DROP_ENERGY: f64 = 0.6;
const FLASH_HZ: f64 = 8.0;
```

After `music_centroid`:

```rust
/// Open up as the music builds and flash when it comes back in after a lull.
///
/// `build` is the seconds the light takes to follow the energy, `flash` how long the flash
/// lasts. The thresholds are tuned on synthetic music, not on real tracks.
pub fn music_drop(
    source: Arc<AudioSource>,
    color: Color,
    flash: f64,
    build: f64,
    delay: f64,
    sensitivity: f64,
) -> Result<Effect> {
    if flash <= 0.0 {
        return Err(invalid("flash must be positive"));
    }
    if build <= 0.0 {
        return Err(invalid("build must be positive"));
    }
    source.set_delay(delay)?;
    source.set_sensitivity(sensitivity)?;
    let mut fast = 0.0f64;
    let mut slow = 0.0f64;
    let mut lull = 0.0f64;
    let mut lulled = false;
    let mut seen = source.heard().beats;
    let mut began = f64::NEG_INFINITY;
    let mut last = 0.0;
    Ok(Box::new(move |t| {
        let step = (t - last).max(0.0);
        last = t;
        let heard = source.heard();
        let energy = heard.levels.loudness();
        fast += (energy - fast) * (1.0 - (-step / build).exp());
        slow += (energy - slow) * (1.0 - (-step / DROP_SLOW).exp());
        if slow > LULL_FLOOR && fast < LULL_RATIO * slow {
            lull += step;
            if lull >= LULL_HOLD {
                lulled = true;
            }
        } else {
            lull = 0.0;
            if fast >= slow {
                lulled = false; // the music came back gently: a later beat is not a drop
            }
        }
        let arrived = heard.beats != seen;
        seen = heard.beats;
        if arrived && lulled && energy >= DROP_ENERGY {
            lulled = false;
            began = t;
        }
        if t - began < flash {
            return if ((t - began) * FLASH_HZ).rem_euclid(1.0) < 0.5 { color } else { OFF };
        }
        color.scaled(0.15 + 0.6 * fast)
    }))
}
```

Add `WHITE` to the `crate::color` import. In `catalog.rs`: add `WHITE` to `use crate::color::{parse_color, Color, BLUE, GREEN, RED};`, add to `range()`: `"flash" => (0.1, 2.0, 0.05),` and `"build" => (0.5, 10.0, 0.5),`, and after the `centroid` entry:

```rust
    EffectInfo {
        name: "drop",
        summary: "opens up as the music builds, flashes when it drops back in",
        needs: Some(Needs::Audio),
        defaults: || {
            vec![
                ("color", Param::Color(Some(WHITE))),
                ("flash", Param::Number(0.4)),
                ("build", Param::Number(3.0)),
                ("delay", Param::Number(0.0)),
                ("sensitivity", Param::Number(DEFAULT_SENSITIVITY)),
            ]
        },
        build: |p, s| {
            audio::music_drop(
                audio(s, "drop")?,
                colour(p, "color"),
                n(p, "flash"),
                n(p, "build"),
                n(p, "delay"),
                n(p, "sensitivity"),
            )
        },
        ranges: &[],
    },
```

- [ ] **Step 8: Run the Rust checks**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all pass.

- [ ] **Step 9: Commit**

```bash
git add src/chsmartbulb/music.py src/chsmartbulb/catalog.py tests/test_catalog_and_music.py crates/core/src/audio.rs crates/core/src/catalog.rs
git commit -m "feat: drop opens up as the music builds and flashes when it comes back in"
```

---

### Task 5: `ambient`

**Files:**
- Modify: `src/chsmartbulb/music.py`, `src/chsmartbulb/catalog.py`, `crates/core/src/audio.rs`, `crates/core/src/catalog.rs`
- Test: `tests/test_catalog_and_music.py`

**Interfaces:**
- Consumes: `_set_delay`, `_check_sensitivity`, `WHITE` (Python; add `from .effects import WARM` as a runtime import beside the `TYPE_CHECKING` one); `WARM` (`crate::effects::WARM`), `WHITE` (Rust).
- Produces: `music.music_ambient(source, base=WARM, accent=WHITE, period=6.0, decay=5.0, delay=0.0, sensitivity=DEFAULT_SENSITIVITY) -> Effect`; `audio::music_ambient(source: Arc<AudioSource>, base: Color, accent: Color, period: f64, decay: f64, delay: f64, sensitivity: f64) -> Result<Effect>`.

- [ ] **Step 1: Write the failing Python tests**

```python
def test_ambient_breathes_and_never_goes_dark():
    source = FakeMusic()
    ambient = catalog.create("ambient", {"base": "c80000", "accent": "0000ff", "period": 6.0, "decay": 5.0}, audio=source)
    assert ambient(0.0) == Color(r=50)  # never beat and silent: 200 * 0.25
    assert ambient(3.0) == Color(r=100)  # half way through the swell: 200 * 0.5
    source.beat(at=10.0)
    source.now = 10.0
    assert ambient(3.0) == Color(b=255)  # a beat shows the accent
    source.now = 20.0
    assert ambient(3.0) == Color(r=100)  # and the breathing is back


def test_ambient_refuses_what_does_not_fit():
    source = FakeMusic()
    for params in ({"period": 0}, {"decay": 0}, {"sensitivity": 2}, {"delay": 5}):
        with pytest.raises(ValueError):
            catalog.create("ambient", params, audio=source)
```

Add `"ambient": "audio",` after `"drop": "audio",` in the `needs` dict.

- [ ] **Step 2: Run to see them fail**

Run: `.venv/Scripts/python -m pytest tests/test_catalog_and_music.py -q -k ambient`
Expected: FAIL (`unknown effect 'ambient'`).

- [ ] **Step 3: Implement the Python side**

In `music.py`, beside the `from .errors import SmartBulbError` line add `from .effects import WARM` (keep `Effect` under `TYPE_CHECKING`; import order as ruff's `I` rule asks). After `music_drop`:

```python
def music_ambient(
    source: AudioSource,
    base: Color = WARM,
    accent: Color = WHITE,
    period: float = 6.0,
    decay: float = 5.0,
    delay: float = 0.0,
    sensitivity: float = DEFAULT_SENSITIVITY,
) -> Effect:
    """A calm colour that breathes every ``period`` seconds, with ``accent`` flashing on the beats.

    Never dark: in silence it is only the breathing.
    """
    if period <= 0.0:
        raise ValueError("period must be positive")
    if decay <= 0.0:
        raise ValueError("decay must be positive")
    _set_delay(source, delay)
    _check_sensitivity(source, sensitivity)

    def effect(t: float) -> Color:
        swell = 0.5 - 0.5 * math.cos(2.0 * math.pi * t / period)
        flash = math.exp(-decay * (source.clock() - source.last_beat))
        return base.scaled(0.25 + 0.25 * swell).mix(accent, flash)

    return effect
```

In `catalog.py` (importing `WARM` through `effects.WARM`, which the module already has), after the `drop` entry:

```python
    EffectInfo(
        "ambient",
        "a calm colour that breathes, an accent on the beats; never dark",
        music.music_ambient,
        {
            "base": effects.WARM,
            "accent": WHITE,
            "period": 6.0,
            "decay": 5.0,
            "delay": 0.0,
            "sensitivity": music.DEFAULT_SENSITIVITY,
        },
        needs="audio",
    ),
```

- [ ] **Step 4: Run the Python tests**

Run: `.venv/Scripts/python -m pytest tests -q` and `.venv/Scripts/python -m ruff check`
Expected: all pass (a circular-import error would mean `effects.py` grew an import of `music`; it has none today).

- [ ] **Step 5: Write the failing Rust tests**

```rust
    #[test]
    fn ambient_breathes_and_never_goes_dark() {
        let (clock, now) = manual_clock();
        let source = AudioSource::new(clock);
        let mut effect = music_ambient(source.clone(), Color::rgb(200, 0, 0), BLUE, 6.0, 5.0, 0.0, 0.5).unwrap();
        assert_eq!(effect(0.0), Color::rgb(50, 0, 0)); // never beat and silent: 200 * 0.25
        assert_eq!(effect(3.0), Color::rgb(100, 0, 0)); // half way through the swell
        set(&now, 10.0);
        source.publish(Levels::default(), 10.0);
        assert_eq!(effect(3.0), BLUE); // a beat shows the accent
        set(&now, 20.0);
        assert_eq!(effect(3.0), Color::rgb(100, 0, 0)); // and the breathing is back
    }

    #[test]
    fn ambient_refuses_what_does_not_fit() {
        let (clock, _) = manual_clock();
        let source = AudioSource::new(clock);
        for (period, decay, delay, sensitivity) in
            [(0.0, 5.0, 0.0, 0.5), (6.0, 0.0, 0.0, 0.5), (6.0, 5.0, 5.0, 0.5), (6.0, 5.0, 0.0, 2.0)]
        {
            assert!(music_ambient(source.clone(), WARM, WHITE, period, decay, delay, sensitivity).is_err());
        }
    }
```

In `catalog.rs` change the `describe` count `17` to `18`.

- [ ] **Step 6: Run to see them fail**

Run: `cargo test -p chsmartbulb-core ambient`
Expected: FAIL to compile.

- [ ] **Step 7: Implement the Rust side**

In `audio.rs`, change `use crate::effects::Effect;` to `use crate::effects::{Effect, WARM};`. After `music_drop`:

```rust
/// A calm colour that breathes every `period` seconds, with `accent` flashing on the beats.
///
/// Never dark: in silence it is only the breathing.
pub fn music_ambient(
    source: Arc<AudioSource>,
    base: Color,
    accent: Color,
    period: f64,
    decay: f64,
    delay: f64,
    sensitivity: f64,
) -> Result<Effect> {
    if period <= 0.0 {
        return Err(invalid("period must be positive"));
    }
    if decay <= 0.0 {
        return Err(invalid("decay must be positive"));
    }
    source.set_delay(delay)?;
    source.set_sensitivity(sensitivity)?;
    Ok(Box::new(move |t| {
        let heard = source.heard();
        let swell = 0.5 - 0.5 * (2.0 * PI * t / period).cos();
        let flash = (-decay * (heard.now - heard.last_beat)).exp();
        base.scaled(0.25 + 0.25 * swell).mix(&accent, flash)
    }))
}
```

In `catalog.rs`, after the `drop` entry (`WARM` and `WHITE` are imported there after Task 4):

```rust
    EffectInfo {
        name: "ambient",
        summary: "a calm colour that breathes, an accent on the beats; never dark",
        needs: Some(Needs::Audio),
        defaults: || {
            vec![
                ("base", Param::Color(Some(WARM))),
                ("accent", Param::Color(Some(WHITE))),
                ("period", Param::Number(6.0)),
                ("decay", Param::Number(5.0)),
                ("delay", Param::Number(0.0)),
                ("sensitivity", Param::Number(DEFAULT_SENSITIVITY)),
            ]
        },
        build: |p, s| {
            audio::music_ambient(
                audio(s, "ambient")?,
                colour(p, "base"),
                colour(p, "accent"),
                n(p, "period"),
                n(p, "decay"),
                n(p, "delay"),
                n(p, "sensitivity"),
            )
        },
        ranges: &[],
    },
```

- [ ] **Step 8: Run the Rust checks**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: all pass.

- [ ] **Step 9: Commit**

```bash
git add src/chsmartbulb/music.py src/chsmartbulb/catalog.py tests/test_catalog_and_music.py crates/core/src/audio.rs crates/core/src/catalog.rs
git commit -m "feat: ambient breathes a calm colour and flashes an accent on the beats"
```

---

### Task 6: Documentation and the whole check

**Files:**
- Modify: `docs/usage.md`

**Interfaces:** none.

- [ ] **Step 1: Add the table rows**

In the effect table of `docs/usage.md`, after the `stereo` row add:

```
| `beathue` | Turn the colour on every beat, glow with the bass | `step`, `decay`, `floor`, `saturation`, `delay`, `sensitivity` |
| `tempo` | Warm for slow music, cool for fast, by the gaps between beats | `slow`, `fast`, `smoothing`, `delay`, `sensitivity` |
| `centroid` | Blend two colours by whether the sound is bass-heavy or bright | `low`, `high`, `width`, `release`, `delay` |
| `drop` | Open up as the music builds, flash when it comes back in | `color`, `flash`, `build`, `delay`, `sensitivity` |
| `ambient` | A calm colour that breathes, an accent on the beats | `base`, `accent`, `period`, `decay`, `delay`, `sensitivity` |
```

- [ ] **Step 2: Describe them**

In "### Sound-reactive effects", change "`music`, `spectrum`, `volume` and `stereo` analyse" to "`music`, `spectrum`, `volume`, `stereo`, `beathue`, `tempo`, `centroid`, `drop` and `ambient` analyse", and after the `stereo` example (the `chsmartbulb effect stereo ...` block) add:

```markdown
`beathue` turns the colour by `step` degrees on every beat and never falls below `floor` of its
brightness, where `music` goes dark between beats.

`tempo` estimates the speed of the music from the gaps between beats and shows `slow` bpm as a warm
colour and `fast` bpm as a cool one. It is only as good as the beat detection: music without a clear
beat leaves it at the last tempo it heard.

`centroid` shows `low` for bass-heavy sound and `high` for bright sound; `width` stretches the
measured position, as it does for `stereo`.

`drop` opens up slowly as the music gets louder and flashes in `color` for `flash` seconds when
loud music comes back in after a lull, then settles to a glow. Its thresholds were tuned on synthetic
signals, so on some music it flashes too often or not at all; `sensitivity` changes which beats count.

`ambient` is a calm `base` colour breathing every `period` seconds, with `accent` flashing on each
beat. In silence it keeps breathing.

```bash
chsmartbulb effect tempo -s slow=70 -s fast=140
chsmartbulb effect ambient -s base=ff6e14 -s accent=ffffff
```
```

In the agent paragraph, change "`music` and `spectrum` follow its feed" to "the sound effects follow its feed".

- [ ] **Step 3: Run the whole check**

Run each and expect success:

```bash
.venv/Scripts/python -m ruff check
.venv/Scripts/python -m pytest -q
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Then confirm both catalogs list the same names in the same order:

```bash
.venv/Scripts/python -c "from chsmartbulb import catalog; print(','.join(catalog.CATALOG))"
grep -o 'name: "[a-z]*"' crates/core/src/catalog.rs | head -18 | sed 's/name: //; s/"//g' | tr '\n' ','
```
Expected: the same 18 names, `...,stereo,beathue,tempo,centroid,drop,ambient,screen`.

- [ ] **Step 4: Commit**

```bash
git add docs/usage.md
git commit -m "docs: the five new music effects"
```
