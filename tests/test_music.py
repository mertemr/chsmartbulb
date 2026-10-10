"""Named effects and the sound analysis behind the reactive ones."""

import asyncio
import math
import sys
import types
from typing import ClassVar

import pytest

from chsmartbulb import music
from chsmartbulb.errors import SmartBulbError

np = pytest.importorskip("numpy")


def tone(hz, seconds, amplitude=0.5, rate=music.RATE):
    t = np.arange(int(rate * seconds)) / rate
    return (amplitude * np.sin(2 * math.pi * hz * t) * 32767).astype("<i2").tobytes()


def silence(seconds, rate=music.RATE):
    return bytes(2 * int(rate * seconds))


def feed(source, pcm):
    """Feed audio one block at a time, as a capture would."""
    step = music.BLOCK * 2
    for i in range(0, len(pcm), step):
        source.feed(pcm[i : i + step])


def test_levels_follow_the_band_that_is_playing():
    source = music.MusicSource()
    assert source.levels == music.Levels()
    feed(source, tone(100, 0.5))
    assert source.levels.bass > 0.9
    assert source.levels.mid < 0.1
    assert source.levels.treble < 0.1
    feed(source, tone(4000, 0.5))
    assert source.levels.treble > 0.5  # the abrupt switch itself is the recent peak
    assert source.levels.bass < 0.1
    feed(source, silence(0.5))
    assert source.levels == music.Levels()


def test_levels_adapt_to_quiet_playback():
    source = music.MusicSource()
    feed(source, tone(100, 1.0, amplitude=0.01))
    assert source.levels.bass > 0.9  # relative to the recent peak, not to full scale


def test_gain_holds_through_a_pause():
    source = music.MusicSource()
    feed(source, tone(100, 1.0))
    feed(source, silence(60.0))
    onsets = []
    source.on_block = lambda levels, onset: onsets.append(onset)
    feed(source, tone(100, 0.2, amplitude=0.005))
    assert 0 < source.levels.bass < 0.05  # still measured against the music before the pause
    assert max(onsets) == 0.0  # and too faint to be a beat


def test_balance_follows_the_louder_side():
    source = music.MusicSource(channels=2)
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


def test_on_block_sees_every_block_with_its_onset():
    source = music.MusicSource()
    seen = []
    source.on_block = lambda levels, onset: seen.append((levels.bass > 0.9, onset))
    feed(source, tone(80, 0.1))
    assert seen[0] == (True, 100.0)  # out of silence the rise is as strong as it gets
    assert len(seen) == 4  # 0.1 s is four full blocks


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

    inputs: ClassVar = [
        {"index": 1, "name": "Microphone (Webcam)", "maxInputChannels": 1, "defaultSampleRate": 44100.0},
        {"index": 2, "name": "Headset Microphone", "maxInputChannels": 2, "defaultSampleRate": 48000.0},
    ]

    def get_default_wasapi_loopback(self):
        return self.devices[1]

    def get_default_input_device_info(self):
        return self.inputs[0]

    def get_device_count(self):
        return 8

    def get_device_info_by_index(self, index):
        known = {device["index"]: device for device in self.inputs}
        loopback = {device["index"]: {**device, "isLoopbackDevice": True} for device in self.devices}
        output = {"index": index, "name": "Speakers", "maxInputChannels": 0, "defaultSampleRate": 48000.0}
        return known.get(index) or loopback.get(index) or output

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


def test_missing_numpy_is_reported_clearly(monkeypatch):
    import builtins

    real_import = builtins.__import__

    def no_numpy(name, *args, **kwargs):
        if name == "numpy":
            raise ImportError(name)
        return real_import(name, *args, **kwargs)

    monkeypatch.setattr(builtins, "__import__", no_numpy)
    monkeypatch.setenv("CHSMARTBULB_NATIVE", "0")
    with pytest.raises(SmartBulbError, match="numpy"):
        music.MusicSource()


def test_the_rust_analysis_needs_no_numpy_and_matches_the_python_one(monkeypatch):
    pytest.importorskip("chsmartbulb_native")
    pcm = tone(100, 0.3) + silence(0.2) + tone(3000, 0.2, amplitude=0.1) + (tone(80, 0.1) + silence(0.2)) * 3

    def analysed(native):
        monkeypatch.setenv("CHSMARTBULB_NATIVE", "1" if native else "0")
        seen = []
        source = music.MusicSource(channels=1, mic=False)
        source.on_block = lambda levels, onset: seen.append((levels, onset))
        source.feed(pcm)
        return seen

    rust, python = analysed(True), analysed(False)
    assert len(rust) == len(python) > 20
    for (levels, onset), (expected, expected_onset) in zip(rust, python, strict=True):
        assert levels.bass == pytest.approx(expected.bass, abs=1e-9)
        assert levels.mid == pytest.approx(expected.mid, abs=1e-9)
        assert levels.treble == pytest.approx(expected.treble, abs=1e-9)
        assert onset == pytest.approx(expected_onset, rel=1e-9, abs=1e-9)

    real_import = __import__

    def no_numpy(name, *args, **kwargs):
        if name == "numpy":
            raise ImportError(name)
        return real_import(name, *args, **kwargs)

    monkeypatch.setenv("CHSMARTBULB_NATIVE", "1")
    monkeypatch.setattr("builtins.__import__", no_numpy)
    source = music.MusicSource()
    source.feed(bytes(4 * music.BLOCK))
    assert source.levels == music.Levels()


def test_microphone_capture_learns_the_room_and_ignores_it():
    heard = []
    source = music.MusicSource(mic=True)
    source.on_block = lambda levels, onset: heard.append(levels.bass)
    source.feed(tone(100, 2.0, amplitude=0.01))  # the hum of the room
    assert max(heard) == 0.0
    source.feed(tone(100, 0.2, amplitude=0.5))
    assert heard[-1] > 0.9
    source.feed(tone(100, 0.2, amplitude=0.01))
    assert heard[-1] == 0.0  # the room again, not a faint sound

    played = []
    output = music.MusicSource()  # what plays on the computer has no room in it
    output.on_block = lambda levels, onset: played.append(levels.bass)
    output.feed(tone(100, 0.2, amplitude=0.01))
    assert played[-1] > 0.9


def test_microphone_is_asked_for_from_each_backend(monkeypatch):
    FakePortAudio.instances.clear()
    monkeypatch.setitem(sys.modules, "pyaudiowpatch", types.SimpleNamespace(PyAudio=FakePortAudio, paInt16=8))
    commands = []

    async def no_parec(*command, **options):
        commands.append(command)
        raise FileNotFoundError

    monkeypatch.setattr(asyncio, "create_subprocess_exec", no_parec)

    async def opened(device=music.DEFAULT_DEVICE):
        source = music.MusicSource(backend="wasapi", mic=True, device=device)
        await source.start()
        await asyncio.sleep(0.01)
        chosen = FakePortAudio.instances[-1].opened
        await source.stop()
        return chosen["input_device_index"], chosen["channels"], chosen["rate"]

    async def scenario():
        assert await opened() == (1, 1, 44100)  # the default input, not the loopback of the output
        assert await opened(device="headset") == (2, 2, 48000)
        missing = music.MusicSource(backend="wasapi", mic=True, device="speakers")
        await missing.start()
        with pytest.raises(SmartBulbError, match=r"no microphone matches 'speakers'.*Webcam"):
            await missing.wait()

        for device in (music.DEFAULT_DEVICE, "alsa_input.usb"):
            source = music.MusicSource(backend="parec", mic=True, device=device)
            await source.start()
            with pytest.raises(SmartBulbError, match="parec"):
                await source.wait()

    asyncio.run(scenario())
    assert [command[1] for command in commands] == ["--device=@DEFAULT_SOURCE@", "--device=alsa_input.usb"]


def test_cli_passes_the_microphone_choice_on():
    from chsmartbulb import cli

    args = cli.build_parser().parse_args(["--mic", "audio-agent"])
    assert cli._music(args).keywords["mic"] is True
    assert cli._music(cli.build_parser().parse_args(["audio-agent"])).keywords["mic"] is False
