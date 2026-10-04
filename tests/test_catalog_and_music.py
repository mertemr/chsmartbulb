"""Named effects and the sound analysis behind the reactive ones."""

import asyncio
import math
import sys
import types
from typing import ClassVar

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
    needs = {entry["name"]: entry["needs"] for entry in listing if entry["needs"]}
    assert needs == {"music": "audio", "spectrum": "audio", "volume": "audio", "stereo": "audio", "screen": "screen"}


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


def test_gain_holds_through_a_pause():
    clock = Clock()
    source = music.MusicSource(clock=clock)
    feed(source, clock, tone(100, 1.0))
    feed(source, clock, silence(60.0))
    beats = source.beats
    feed(source, clock, tone(100, 0.2, amplitude=0.005))
    assert 0 < source.levels.bass < 0.05  # still measured against the music before the pause
    assert source.beats == beats  # and too faint to be a beat


def test_beats_are_counted_once_per_kick():
    clock = Clock()
    source = music.MusicSource(clock=clock)
    kick = tone(80, 0.1) + silence(0.4)  # 2 Hz
    feed(source, clock, kick * 6)
    assert source.beats == 6
    assert clock.now - source.last_beat == pytest.approx(0.5, abs=0.06)


def test_sensitivity_decides_which_rises_are_beats():
    def beats(sensitivity):
        clock = Clock()
        source = music.MusicSource(clock=clock)
        source.sensitivity = sensitivity
        feed(source, clock, tone(80, 1.0, amplitude=0.2))
        accent = tone(80, 0.1, amplitude=0.5) + tone(80, 0.4, amplitude=0.2)  # 2.5 times the bass under it
        feed(source, clock, accent * 4)
        return source.beats

    assert music.beat_ratio(music.DEFAULT_SENSITIVITY) == 1.5
    assert beats(0.5) == 5
    assert beats(0.0) == 1  # only the start out of silence is hard enough
    assert beats(1.0) >= 5


def test_balance_follows_the_louder_side():
    source = music.MusicSource(channels=2, clock=Clock())
    assert source.levels.balance == 0.0

    def play(left, right):
        t = np.arange(music.BLOCK) / music.RATE
        wave = np.sin(2 * math.pi * 200 * t) * 32767
        source.feed(np.stack([left * wave, right * wave], axis=1).astype("<i2").tobytes())
        return source.levels

    assert play(0.5, 0.0).balance == pytest.approx(-1.0)
    assert play(0.2, 0.4).balance == pytest.approx(1 / 3, abs=0.01)
    assert play(0.3, 0.3).balance == pytest.approx(0.0, abs=0.01)
    assert play(0.3, 0.3).bass > 0.5  # the bands come from both sides together
    assert play(0.0, 0.0) == music.Levels()


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
    remote.push(music.Levels(bass=1.0), 8.0)
    assert (remote.levels, remote.beats) == (music.Levels(), 0)
    clock.now = 0.25
    remote.push(music.Levels(), 0.0)
    assert (remote.levels.bass, remote.beats, remote.last_beat) == (1.0, 1, 0.2)
    remote.clear()
    assert remote.levels == music.Levels()
    clock.now = 1.0
    remote.push(music.Levels(mid=0.5), 0.0)
    assert remote.levels == music.Levels()  # the block pushed before the clear is gone, this one is still held


def test_on_block_sees_every_block_before_any_delay():
    clock = Clock()
    source = music.MusicSource(clock=clock)
    source.delay = 1.0
    seen = []
    source.on_block = lambda levels, onset: seen.append((levels.bass > 0.9, onset))
    feed(source, clock, tone(80, 0.1))
    assert seen[0] == (True, 100.0)  # out of silence the rise is as strong as it gets
    assert len(seen) == 4  # 0.1 s is four full blocks
    assert source.beats == 0  # the source's own view is still held back


def test_capture_backend_follows_what_the_machine_has(monkeypatch):
    source = music.MusicSource()
    monkeypatch.setattr(music.shutil, "which", lambda name: "/usr/bin/parec")
    assert source._pick_backend() == "parec"
    monkeypatch.setattr(music.shutil, "which", lambda name: None)
    monkeypatch.setattr(music.sys, "platform", "linux")
    assert source._pick_backend() == "soundcard"
    monkeypatch.setattr(music.sys, "platform", "win32")
    assert source._pick_backend() == "wasapi"
    assert music.MusicSource(backend="parec")._pick_backend() == "parec"
    with pytest.raises(ValueError, match="backend"):
        music.MusicSource(backend="alsa")


class FakePortAudio:
    """The slice of PyAudioWPatch the WASAPI backend uses, with a 48 kHz stereo output."""

    paInt16 = 8
    paContinue = 0
    devices: ClassVar = [
        {"index": 3, "name": "Headphones [Loopback]", "maxInputChannels": 2, "defaultSampleRate": 44100.0},
        {"index": 7, "name": "Speakers (USB DAC) [Loopback]", "maxInputChannels": 2, "defaultSampleRate": 48000.0},
    ]
    instances: ClassVar[list] = []

    class Stream:
        def __init__(self):
            self.active = True
            self.closed = False

        def is_active(self):
            return self.active

        def stop_stream(self):
            self.active = False

        def close(self):
            self.closed = True

    def __init__(self):
        self.opened = None
        self.callback = None
        self.stream = None
        self.terminated = False
        FakePortAudio.instances.append(self)

    def get_default_wasapi_loopback(self):
        return self.devices[1]

    def get_loopback_device_info_generator(self):
        return iter(self.devices)

    def open(self, **options):
        self.opened = options
        self.callback = options["stream_callback"]
        self.stream = FakePortAudio.Stream()
        return self.stream

    def terminate(self):
        self.terminated = True


def test_wasapi_capture_mixes_down_and_analyses_at_the_device_rate(monkeypatch):
    FakePortAudio.instances.clear()
    fake = types.SimpleNamespace(PyAudio=FakePortAudio, paInt16=8, paContinue=0)
    monkeypatch.setitem(sys.modules, "pyaudiowpatch", fake)

    async def scenario():
        source = music.MusicSource(backend="wasapi")
        await source.start()
        await asyncio.sleep(0.01)
        portaudio = FakePortAudio.instances[-1]
        assert portaudio.opened["rate"] == 48000
        assert portaudio.opened["channels"] == 2
        assert portaudio.opened["input_device_index"] == 7
        assert portaudio.opened["frames_per_buffer"] == 1024  # twice the block of the 22 kHz capture

        t = np.arange(1024 * 8) / 48000
        left = (0.5 * np.sin(2 * math.pi * 100 * t) * 32767).astype("<i2")
        stereo = np.stack([left, left // 2], axis=1).tobytes()
        assert portaudio.callback(stereo, 1024 * 8, None, 0) == (None, 0)
        await asyncio.sleep(0.01)
        assert source.levels.bass > 0.9
        assert source.levels.treble < 0.1
        assert source.levels.balance == pytest.approx(-1 / 3, abs=0.01)

        await source.stop()
        assert (portaudio.stream.active, portaudio.stream.closed, portaudio.terminated) == (False, True, True)

    asyncio.run(scenario())


def test_wasapi_capture_finds_an_output_by_name_and_reports_a_bad_one(monkeypatch):
    FakePortAudio.instances.clear()
    fake = types.SimpleNamespace(PyAudio=FakePortAudio, paInt16=8, paContinue=0)
    monkeypatch.setitem(sys.modules, "pyaudiowpatch", fake)

    async def scenario():
        source = music.MusicSource(backend="wasapi", device="headphones")
        await source.start()
        await asyncio.sleep(0.01)
        assert FakePortAudio.instances[-1].opened["rate"] == 44100
        await source.stop()

        missing = music.MusicSource(backend="wasapi", device="hdmi")
        await missing.start()
        with pytest.raises(SmartBulbError, match="no output matches 'hdmi'; available: Headphones"):
            await missing.wait()
        assert FakePortAudio.instances[-1].terminated

    asyncio.run(scenario())


def test_wasapi_capture_goes_silent_when_the_output_stops_delivering(monkeypatch):
    FakePortAudio.instances.clear()
    fake = types.SimpleNamespace(PyAudio=FakePortAudio, paInt16=8, paContinue=0)
    monkeypatch.setitem(sys.modules, "pyaudiowpatch", fake)
    monkeypatch.setattr(music, "_STREAM_POLL", 0.001)

    async def scenario():
        source = music.MusicSource(backend="wasapi")
        seen = []
        source.on_block = lambda levels, beat: seen.append(levels)
        await source.start()
        await asyncio.sleep(0)
        t = np.arange(1024) / 48000
        left = (0.5 * np.sin(2 * math.pi * 100 * t) * 32767).astype("<i2")
        FakePortAudio.instances[-1].callback(np.stack([left, left], axis=1).tobytes(), 1024, None, 0)
        await asyncio.sleep(0.02)  # the player closed its stream: WASAPI calls back no more
        assert seen[0].bass > 0.9
        assert source.levels == music.Levels()
        await source.stop()

    asyncio.run(scenario())


def test_wasapi_capture_reports_a_stream_that_dies(monkeypatch):
    FakePortAudio.instances.clear()
    fake = types.SimpleNamespace(PyAudio=FakePortAudio, paInt16=8, paContinue=0)
    monkeypatch.setitem(sys.modules, "pyaudiowpatch", fake)
    monkeypatch.setattr(music, "_STREAM_POLL", 0.001)

    async def scenario():
        source = music.MusicSource(backend="wasapi")
        await source.start()
        await asyncio.sleep(0.005)
        FakePortAudio.instances[-1].stream.active = False  # the output device went away
        with pytest.raises(SmartBulbError, match="stopped unexpectedly"):
            await source.wait()

    asyncio.run(scenario())


def test_soundcard_capture_feeds_the_analysis_and_names_its_failures(monkeypatch):
    class Recorder:
        def __init__(self, frames):
            self.frames = frames

        def __enter__(self):
            return self

        def __exit__(self, *exc):
            return False

        def record(self, numframes):
            if not self.frames:
                raise AssertionError  # how soundcard reports an output format it cannot handle
            return self.frames.pop(0)

    class Device:
        name = "Speakers"

        def __init__(self, frames):
            self.frames = frames

        def recorder(self, samplerate, channels, blocksize):
            return Recorder(self.frames)

    t = np.arange(music.BLOCK) / music.RATE
    loud = (0.5 * np.sin(2 * math.pi * 100 * t)).astype("float32").reshape(-1, 1)
    asked = []

    def get_microphone(id, include_loopback):
        asked.append((id, include_loopback))
        return Device([loud, loud, loud])

    fake = types.SimpleNamespace(default_speaker=lambda: Device([]), get_microphone=get_microphone)
    monkeypatch.setitem(sys.modules, "soundcard", fake)

    async def scenario():
        source = music.MusicSource(backend="soundcard")
        await source.start()
        with pytest.raises(SmartBulbError, match="sound capture failed: AssertionError") as caught:
            await source.wait()
        assert isinstance(caught.value.__cause__, AssertionError)
        await asyncio.sleep(0)  # let the last queued blocks reach the analysis
        return source

    source = asyncio.run(scenario())
    assert asked == [("Speakers", True)]  # the default output's loopback
    assert source.levels.bass > 0.9


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


def test_sensitivity_is_set_through_the_effect_parameters():
    source = FakeMusic()
    catalog.create("music", {"sensitivity": 0.2}, audio=source)
    assert source.sensitivity == 0.2
    catalog.create("music", {}, audio=source)
    assert source.sensitivity == 0.5
    with pytest.raises(ValueError, match="sensitivity"):
        catalog.create("music", {"sensitivity": 2}, audio=source)


def test_music_volume_shows_one_colour_as_bright_as_the_sound():
    source = FakeMusic()
    volume = catalog.create("volume", {"color": "0000ff", "release": 2.0}, audio=source)
    assert volume(0.0) == Color()
    source.levels = music.Levels(mid=1.0)
    assert volume(0.1) == Color(b=255)
    source.levels = music.Levels()
    assert volume(0.35) == Color(b=64)  # fell from 1.0 to 0.5, squared
    assert volume(2.0) == Color()
    glow = catalog.create("volume", {"color": "0000ff", "floor": 0.2}, audio=source)
    assert glow(0.0) == Color(b=51)  # never darker than the floor
    with pytest.raises(ValueError, match="floor"):
        catalog.create("volume", {"floor": 2}, audio=source)


def test_music_stereo_blends_two_colours_by_position():
    source = FakeMusic()
    stereo = catalog.create("stereo", {"left": "00ff00", "right": "ff0000", "width": 2}, audio=source)
    assert stereo(0.0) == Color()  # silent
    source.levels = music.Levels(bass=1.0)
    assert stereo(1.0) == Color(r=128, g=128)  # centred: an even mix
    source.levels = music.Levels(bass=1.0, balance=-0.5)
    assert stereo(1.05).g > stereo(1.05).r > 0  # on its way to the left colour
    assert stereo(3.0) == Color(g=255)  # half way out is all the way with width 2
    source.levels = music.Levels(bass=1.0, balance=0.25)
    assert stereo(5.0) == Color(r=191, g=64)
    source.levels = music.Levels()
    assert stereo(5.2) == Color(r=31, g=10)  # faded to 0.4 squared, and the position holds in silence
    source.levels = music.Levels(bass=1.0, balance=-0.5)
    assert stereo(9.0) == Color(g=255)  # out of the dark a sound shows where it is at once
    with pytest.raises(ValueError, match="width"):
        catalog.create("stereo", {"width": 0}, audio=source)


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
