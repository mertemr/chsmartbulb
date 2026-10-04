"""Named effects and the sound analysis behind the reactive ones."""

import math

import pytest

from chsmartbulb import Color, catalog, effects, music
from chsmartbulb.errors import SmartBulbError
from fakes import FakeMusic

np = pytest.importorskip("numpy")


def tone(hz, seconds, amplitude=0.5, rate=music.RATE):
    t = np.arange(int(rate * seconds)) / rate
    return (amplitude * np.sin(2 * math.pi * hz * t) * 32767).astype("<i2").tobytes()


def silence(seconds, rate=music.RATE):
    return bytes(2 * int(rate * seconds))


class Clock:
    def __init__(self):
        self.now = 0.0

    def __call__(self):
        return self.now


def feed(source, clock, pcm):
    """Feed audio one block at a time, advancing the clock as real capture would."""
    step = music.BLOCK * 2
    for i in range(0, len(pcm), step):
        source.feed(pcm[i : i + step])
        clock.now += music.BLOCK / music.RATE


def test_catalog_builds_effects_from_plain_parameters():
    breathe = catalog.create("breathe", {"color": "#00ff00", "period": 2})
    assert breathe(1.0) == Color(g=255)
    drift = catalog.create("palette", {"colors": "red,blue", "hold": 1, "fade_in": 0})
    assert (drift(0.5), drift(1.5)) == (Color(r=255), Color(b=255))
    assert catalog.create("police")(0.1) == Color(r=255)
    assert catalog.resolve("hue")["period"] == 10.0


def test_catalog_rejects_unknown_names_parameters_and_values():
    bad = (("disco", {}), ("hue", {"colour": 1}), ("breathe", {"period": "fast"}), ("palette", {"colors": ""}))
    for name, params in bad:
        with pytest.raises(ValueError):
            catalog.create(name, params)
    with pytest.raises(ValueError, match="audio source"):
        catalog.create("music")


def test_describe_is_plain_data_covering_every_effect():
    listing = catalog.describe()
    assert {entry["name"] for entry in listing} == set(catalog.CATALOG)
    breathe = next(entry for entry in listing if entry["name"] == "breathe")
    assert breathe["params"] == {"color": "#ff0000", "period": 4.0, "floor": 0.0}
    assert [entry["name"] for entry in listing if entry["needs_audio"]] == ["music", "spectrum"]


def test_candle_flickers_within_its_depth_and_palette_blends():
    flame = effects.candle(Color(r=200), depth=0.5)
    levels = [flame(t / 20).r for t in range(200)]
    assert 100 <= min(levels) < max(levels) <= 200
    assert max(levels) - min(levels) > 50
    drift = effects.palette([Color(r=255), Color(b=255)], hold=1, fade_in=1)
    assert drift(2.5) == Color(r=128, b=128)  # fade to red, hold, then half way to blue


def test_levels_follow_the_band_that_is_playing():
    clock = Clock()
    source = music.MusicSource(clock=clock)
    assert source.levels == music.Levels()
    feed(source, clock, tone(100, 0.5))
    assert source.levels.bass > 0.9
    assert source.levels.mid < 0.1
    assert source.levels.treble < 0.1
    feed(source, clock, tone(4000, 0.5))
    assert source.levels.treble > 0.5  # the abrupt switch itself is the recent peak
    assert source.levels.bass < 0.1
    feed(source, clock, silence(0.5))
    assert source.levels == music.Levels()


def test_levels_adapt_to_quiet_playback():
    clock = Clock()
    source = music.MusicSource(clock=clock)
    feed(source, clock, tone(100, 1.0, amplitude=0.01))
    assert source.levels.bass > 0.9  # relative to the recent peak, not to full scale


def test_beats_are_counted_once_per_kick():
    clock = Clock()
    source = music.MusicSource(clock=clock)
    kick = tone(80, 0.1) + silence(0.4)  # 2 Hz
    feed(source, clock, kick * 6)
    assert source.beats == 6
    assert clock.now - source.last_beat == pytest.approx(0.5, abs=0.06)


def test_steady_bass_and_silence_produce_no_beats():
    clock = Clock()
    source = music.MusicSource(clock=clock)
    feed(source, clock, tone(80, 3.0))
    assert source.beats == 1  # only the onset
    feed(source, clock, silence(2.0))
    assert source.beats == 1


def test_delay_holds_levels_and_beats_back():
    clock = Clock()
    source = music.MusicSource(clock=clock)
    source.delay = 0.3
    feed(source, clock, tone(80, 0.1))
    assert (source.levels, source.beats) == (music.Levels(), 0)  # heard, not shown yet
    feed(source, clock, silence(0.15))
    assert source.beats == 0
    feed(source, clock, silence(0.1))
    assert source.beats == 1
    assert source.last_beat == pytest.approx(0.3, abs=0.03)  # the kick started at 0
    assert source.levels.bass > 0.9  # still showing the kick while the capture is already silent
    feed(source, clock, silence(0.4))
    assert source.levels == music.Levels()


def test_remote_audio_applies_delay_and_clears():
    clock = Clock()
    remote = music.RemoteAudio(clock=clock)
    remote.delay = 0.2
    remote.push(music.Levels(bass=1.0), True)
    assert (remote.levels, remote.beats) == (music.Levels(), 0)
    clock.now = 0.25
    remote.push(music.Levels(), False)
    assert (remote.levels.bass, remote.beats, remote.last_beat) == (1.0, 1, 0.2)
    remote.clear()
    assert remote.levels == music.Levels()
    clock.now = 1.0
    remote.push(music.Levels(mid=0.5), False)
    assert remote.levels == music.Levels()  # the block pushed before the clear is gone, this one is still held


def test_on_block_sees_every_block_before_any_delay():
    clock = Clock()
    source = music.MusicSource(clock=clock)
    source.delay = 1.0
    seen = []
    source.on_block = lambda levels, beat: seen.append((levels.bass > 0.9, beat))
    feed(source, clock, tone(80, 0.1))
    assert seen[0] == (True, True)
    assert len(seen) == 4  # 0.1 s is four full blocks
    assert source.beats == 0  # the source's own view is still held back


def test_capture_backend_follows_what_the_machine_has(monkeypatch):
    source = music.MusicSource()
    monkeypatch.setattr(music.shutil, "which", lambda name: "/usr/bin/parec")
    assert source._pick_backend() == "parec"
    monkeypatch.setattr(music.shutil, "which", lambda name: None)
    assert source._pick_backend() == "soundcard"
    assert music.MusicSource(backend="parec")._pick_backend() == "parec"
    with pytest.raises(ValueError, match="backend"):
        music.MusicSource(backend="alsa")


def test_delay_is_set_through_the_effect_parameters():
    source = FakeMusic()
    catalog.create("music", {"delay": 0.25}, audio=source)
    assert source.delay == 0.25
    catalog.create("spectrum", {}, audio=source)
    assert source.delay == 0.0
    with pytest.raises(ValueError, match="delay"):
        catalog.create("music", {"delay": 5}, audio=source)


def test_music_pulse_flashes_on_a_beat_and_changes_hue():
    source = FakeMusic()
    pulse = music.music_pulse(source)
    assert pulse(0.0) == Color()  # silent and no beat yet
    source.beat(at=10.0)
    source.now = 10.0
    first = pulse(0.0)
    assert max(first.r, first.g, first.b) == 255
    source.now = 11.0
    assert max(pulse(1.0).r, pulse(1.0).g, pulse(1.0).b) < 10  # decayed
    source.now = 13.0
    assert pulse(3.0) == Color()  # and fully dark once the music stops
    source.now = 11.0
    source.beat(at=11.0)
    assert pulse(1.0) != first  # new hue
    fixed = music.music_pulse(source, Color(b=255))
    assert fixed(1.0) == Color(b=255)


def test_music_spectrum_maps_bands_to_channels_and_releases_slowly():
    source = FakeMusic()
    spectrum = music.music_spectrum(source, release=2.0)
    source.levels = music.Levels(bass=1.0, mid=0.5, treble=0.0)
    assert spectrum(0.0) == Color(r=255, g=64, b=0)
    source.levels = music.Levels()
    assert spectrum(0.25) == Color(r=64, g=0, b=0)  # red fell from 1.0 to 0.5, squared
    assert spectrum(1.0) == Color()


def test_missing_numpy_is_reported_clearly(monkeypatch):
    import builtins

    real_import = builtins.__import__

    def no_numpy(name, *args, **kwargs):
        if name == "numpy":
            raise ImportError(name)
        return real_import(name, *args, **kwargs)

    monkeypatch.setattr(builtins, "__import__", no_numpy)
    with pytest.raises(SmartBulbError, match="numpy"):
        music.MusicSource()
