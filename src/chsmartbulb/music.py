"""Sound-reactive effects driven by audio captured on the computer.

The sound is taken from the monitor of the default output, so it does not matter
where it plays: laptop speakers, headphones, another Bluetooth device or the bulb.
Needs ``numpy`` (``pip install chsmartbulb[audio]``). Capture uses the ``parec``
tool where it exists (PulseAudio, PipeWire), WASAPI loopback through
``PyAudioWPatch`` on Windows, and the ``soundcard`` package as a last resort.

The analysis can also run on another machine: :class:`RemoteAudio` is fed over the
network by ``chsmartbulb audio-agent``.
"""

from __future__ import annotations

import asyncio
import contextlib
import logging
import math
import os
import shutil
import sys
import threading
import time
from collections import deque
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Protocol

from .color import BLUE, OFF, RED, Color
from .errors import SmartBulbError

if TYPE_CHECKING:
    from collections.abc import Callable

    from .effects import Effect

RATE = 22050
BLOCK = 512  # 23 ms per analysis step
DEFAULT_DEVICE = "@DEFAULT_MONITOR@"
_DEFAULT_MICROPHONE = "@DEFAULT_SOURCE@"  # the same thing, for PulseAudio
BACKENDS = ("auto", "parec", "wasapi", "soundcard")
DEFAULT_SENSITIVITY = 0.5

log = logging.getLogger(__name__)

_BANDS_HZ = ((40, 250), (250, 2000), (2000, 8000))
_GAIN_HALF_LIFE = 4.0  # seconds for the automatic gain to forget a loud passage
_SILENCE = 1e-4
_ROOM_MARGIN = 2.0  # a microphone's sound must stand this many times above the room's noise
_ROOM_DOUBLING = 30.0  # seconds for the estimate of that noise to double while nothing dips below it
_BEAT_GAP = 0.15  # seconds; caps detection at 400 bpm
_BEAT_FLOOR = 0.05  # a beat needs the bass above this fraction of its recent peak
_ONSET_CAP = 100.0
_PAN_SMOOTHING = 0.15  # seconds for the stereo position to settle
_DARK = 1 / 255
MAX_DELAY = 2.0
_STREAM_POLL = 0.5  # seconds between checks that a callback-driven stream is still alive


def _native() -> Any:
    """The Rust analysis from ``chsmartbulb-native`` when it is installed, else ``None``.

    ``CHSMARTBULB_NATIVE=0`` keeps to the Python one.
    """
    if os.environ.get("CHSMARTBULB_NATIVE", "1") == "0":
        return None
    try:
        import chsmartbulb_native
    except ImportError:
        return None
    return chsmartbulb_native


def beat_ratio(sensitivity: float) -> float:
    """How far the bass must rise above its recent average to count as a beat.

    1.5 at the default sensitivity; 5 at 0 (only the hardest hits), barely above 1 at 1.
    """
    return 1.0 + 0.5 * 8.0 ** (1.0 - 2.0 * sensitivity)


@dataclass(frozen=True)
class Levels:
    """Band loudness relative to the recent peak, each 0..1, and where the sound sits."""

    bass: float = 0.0
    mid: float = 0.0
    treble: float = 0.0
    balance: float = 0.0  # -1 all left, +1 all right


class AudioSource(Protocol):
    """What the sound-reactive effects read and the service starts and stops."""

    levels: Levels
    beats: int
    last_beat: float
    delay: float
    sensitivity: float

    def clock(self) -> float: ...

    async def start(self) -> None: ...

    async def stop(self) -> None: ...


class Capture(Protocol):
    """A running analysis whose blocks can be forwarded elsewhere."""

    on_block: Callable[[Levels, float], None] | None

    async def start(self) -> None: ...

    async def wait(self) -> None: ...

    async def stop(self) -> None: ...


class _Published:
    """Levels and beats as the effects see them.

    ``delay`` holds them back by that many seconds. The capture hears the sound
    before a Bluetooth speaker or headphones play it, so without a delay the
    light runs ahead of what you hear. ``sensitivity`` (0..1) decides which
    onsets count as beats, see :func:`beat_ratio`.
    """

    def __init__(self, clock: Callable[[], float]) -> None:
        self._clock = clock
        self._held: deque[tuple[float, Levels, bool]] = deque()
        self._last_onset = -math.inf
        self.delay = 0.0
        self.sensitivity = DEFAULT_SENSITIVITY
        self.levels = Levels()
        self.beats = 0
        self.last_beat = -math.inf

    def clock(self) -> float:
        """The time base :attr:`last_beat` is measured on."""
        return self._clock()

    def _publish(self, levels: Levels, onset: float) -> None:
        """Take one analysed block; ``onset`` is the bass relative to its recent average."""
        now = self.clock()
        beat = onset >= beat_ratio(self.sensitivity) and now - self._last_onset > _BEAT_GAP
        if beat:
            self._last_onset = now
        self._held.append((now + self.delay, levels, beat))
        while self._held and self._held[0][0] <= now:
            due, self.levels, was_beat = self._held.popleft()
            if was_beat:
                self.beats += 1
                self.last_beat = due

    def _clear(self) -> None:
        self._held.clear()
        self.levels = Levels()


class RemoteAudio(_Published):
    """An audio source whose analysis arrives from elsewhere, for example over the network."""

    def __init__(self, *, clock: Callable[[], float] = time.monotonic) -> None:
        super().__init__(clock)

    def push(self, levels: Levels, onset: float) -> None:
        self._publish(levels, onset)

    def clear(self) -> None:
        """Forget what was pending and go silent, for when the feed stops."""
        self._clear()

    async def start(self) -> None:
        """Nothing to start: the feed is not ours to control."""

    async def stop(self) -> None:
        """Nothing to stop."""


class MusicSource(_Published):
    """Analyses PCM audio into band levels and beats.

    :meth:`feed` does the analysis and can be driven by anything; :meth:`start`
    feeds it from the system's audio output, or with ``mic`` from a microphone.
    ``on_block`` is called with every analysed block and its onset strength
    before any delay, which is what the audio agent forwards.

    A microphone hears the room as well as the music, so with ``mic`` the
    steady noise of the room is estimated and taken off each band. Without that
    the automatic gain would turn a quiet room's hiss into a full level.

    With ``chsmartbulb-native`` installed the analysis runs in Rust and needs no
    numpy; otherwise it runs here, with numpy.
    """

    def __init__(
        self,
        *,
        rate: int = RATE,
        block: int = BLOCK,
        channels: int = 1,
        device: str = DEFAULT_DEVICE,
        backend: str = "auto",
        mic: bool = False,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        super().__init__(clock)
        self._rust: Any = _native()
        self._np: Any = None
        try:
            import numpy

            self._np = numpy
        except ImportError:
            if self._rust is None:
                raise SmartBulbError("sound-reactive effects need numpy: pip install chsmartbulb[audio]") from None
        if backend not in BACKENDS:
            raise ValueError(f"unknown audio backend {backend!r}; choose from: {', '.join(BACKENDS)}")
        self._device = device
        self._backend = backend
        self._mic = mic
        self._room = [math.inf, math.inf, math.inf]  # the quietest each band has been lately
        self.on_block: Callable[[Levels, float], None] | None = None
        self._configure(rate, block, channels)
        self._peaks = [0.0, 0.0, 0.0]
        self._bass_average = 0.0
        self._task: asyncio.Task[None] | None = None

    def _configure(self, rate: int, block: int | None = None, channels: int = 1) -> None:
        """Size the analysis for ``rate``; without ``block``, pick one of about 23 ms."""
        if block is None:
            block = BLOCK
            while rate / block > 60:  # keep the bins near 45 Hz wide so the bass band stays resolved
                block *= 2
        self._rate = rate
        self._block = block
        self._channels = channels
        self._size = block * channels * 2  # bytes per analysis step
        self._pending = bytearray()
        if self._rust is not None:
            self._analyzer = self._rust.Analyzer(rate, channels, self._mic, block)
            return
        self._window = self._np.hanning(block)
        hz_per_bin = rate / block
        self._bins = [(max(1, round(lo / hz_per_bin)), round(hi / hz_per_bin)) for lo, hi in _BANDS_HZ]
        self._decay = 0.5 ** (block / rate / _GAIN_HALF_LIFE)
        self._room_rise = 2.0 ** (block / rate / _ROOM_DOUBLING)

    def feed(self, pcm: bytes) -> None:
        """Consume signed 16-bit little-endian samples, interleaved when there are several channels."""
        if self._rust is not None:
            for (bass, mid, treble, balance), onset in self._analyzer.feed(pcm):
                self._deliver(Levels(bass, mid, treble, balance), onset)
            return
        self._pending += pcm
        size = self._size
        while len(self._pending) >= size:
            samples = self._np.frombuffer(bytes(self._pending[:size]), dtype="<i2") / 32768.0
            del self._pending[:size]
            self._analyse(samples.reshape(-1, self._channels))

    def _balance(self, frames: Any) -> float:
        if self._channels < 2:
            return 0.0
        left, right = (float(self._np.sqrt((frames[:, side] ** 2).mean())) for side in (0, 1))
        total = left + right
        return (right - left) / total if total > _SILENCE else 0.0

    def _analyse(self, frames: Any) -> None:
        samples = frames.mean(axis=1)
        spectrum = self._np.abs(self._np.fft.rfft(samples * self._window)) / (self._block / 2)
        values = [float(spectrum[lo:hi].mean()) for lo, hi in self._bins]
        if self._mic:
            values = [self._above_room(band, value) for band, value in enumerate(values)]
        levels = []
        for i, value in enumerate(values):
            heard = value > _SILENCE
            # the gain holds through a pause, or the first faint sound after it would read as full level
            self._peaks[i] = max(value, self._peaks[i] * self._decay) if heard else self._peaks[i]
            levels.append(value / self._peaks[i] if heard else 0.0)

        bass = values[0]
        onset = 0.0
        if levels[0] >= _BEAT_FLOOR:  # something faint after a loud passage is not a beat
            onset = min(_ONSET_CAP, bass / self._bass_average) if self._bass_average > 0.0 else _ONSET_CAP
        # fast enough to catch up with a sustained note before the gap allows another beat
        self._bass_average += 0.25 * (bass - self._bass_average)

        self._deliver(Levels(*levels, balance=self._balance(frames)), onset)

    def _deliver(self, levels: Levels, onset: float) -> None:
        if self.on_block is not None:
            self.on_block(levels, onset)
        self._publish(levels, onset)

    def _above_room(self, band: int, value: float) -> float:
        """What of ``value`` stands out of the room's noise, which is the least the band has held lately."""
        self._room[band] = min(value, max(self._room[band], _SILENCE / 10) * self._room_rise)
        return max(0.0, value - _ROOM_MARGIN * self._room[band])

    async def start(self) -> None:
        """Begin capturing the system's audio output, or the microphone, in the background."""
        if self._task is None:
            self._task = asyncio.create_task(self._capture())

    async def stop(self) -> None:
        task, self._task = self._task, None
        if task is not None:
            task.cancel()
            with contextlib.suppress(asyncio.CancelledError, SmartBulbError):
                await task

    async def wait(self) -> None:
        """Wait for the capture to end; raises if it failed."""
        if self._task is not None:
            await self._task

    def _pick_backend(self) -> str:
        if self._backend != "auto":
            return self._backend
        if shutil.which("parec"):
            return "parec"
        return "wasapi" if sys.platform == "win32" else "soundcard"

    async def _capture(self) -> None:
        backend = self._pick_backend()
        if backend == "parec":
            await self._capture_parec()
        elif backend == "wasapi":
            await self._capture_wasapi()
        else:
            await self._capture_soundcard()

    async def _capture_wasapi(self) -> None:
        """Windows loopback through PyAudioWPatch, at whatever format the output device runs."""
        try:
            import pyaudiowpatch as pyaudio
        except ImportError:
            raise SmartBulbError(
                "sound capture on Windows needs PyAudioWPatch: pip install chsmartbulb[audio]"
            ) from None
        loop = asyncio.get_running_loop()
        audio = pyaudio.PyAudio()
        try:
            device = self._wasapi_microphone(audio) if self._mic else self._wasapi_loopback(audio)
            channels = int(device["maxInputChannels"])
            rate = int(device["defaultSampleRate"])
            self._configure(rate, channels=channels)
            heard = False

            def deliver(pcm: bytes) -> None:
                nonlocal heard
                heard = True
                self.feed(pcm)

            def on_audio(data: bytes, _frames: int, _time: Any, _status: int) -> tuple[None, int]:
                with contextlib.suppress(RuntimeError):  # the loop is gone while shutting down
                    loop.call_soon_threadsafe(deliver, data)
                return None, pyaudio.paContinue

            # Callback mode: a loopback stream delivers nothing while nothing plays, and a blocking read would hang.
            stream = audio.open(
                format=pyaudio.paInt16,
                channels=channels,
                rate=rate,
                input=True,
                input_device_index=int(device["index"]),
                frames_per_buffer=self._block,
                stream_callback=on_audio,
            )
            log.info("capturing %s (%d Hz, %d channels)", device["name"], rate, channels)
            try:
                while stream.is_active():  # PortAudio offers nothing to await
                    await asyncio.sleep(_STREAM_POLL)
                    if not heard:
                        # Nothing has a stream open on the output, so no callback tells us it went quiet.
                        self.feed(bytes(self._size))
                    heard = False
                raise SmartBulbError("sound capture stopped unexpectedly")
            finally:
                stream.stop_stream()
                stream.close()
        except (OSError, LookupError, ValueError) as exc:
            raise SmartBulbError(f"sound capture failed: {type(exc).__name__}: {exc}".rstrip(": ")) from exc
        finally:
            audio.terminate()

    def _wasapi_loopback(self, audio: Any) -> Any:
        """The loopback twin of the default output, or of the output whose name contains the chosen device."""
        if self._device == DEFAULT_DEVICE:
            return audio.get_default_wasapi_loopback()
        wanted = self._device.lower()
        names = []
        for device in audio.get_loopback_device_info_generator():
            names.append(str(device["name"]))
            if wanted in names[-1].lower():
                return device
        raise LookupError(f"no output matches {self._device!r}; available: {', '.join(names) or 'none'}")

    def _wasapi_microphone(self, audio: Any) -> Any:
        """The default input, or the input whose name contains the chosen device."""
        if self._device == DEFAULT_DEVICE:
            return audio.get_default_input_device_info()
        wanted = self._device.lower()
        names = []
        for index in range(audio.get_device_count()):
            device = audio.get_device_info_by_index(index)
            if int(device["maxInputChannels"]) < 1 or device.get("isLoopbackDevice"):
                continue
            names.append(str(device["name"]))
            if wanted in names[-1].lower():
                return device
        raise LookupError(f"no microphone matches {self._device!r}; available: {', '.join(names) or 'none'}")

    async def _capture_soundcard(self) -> None:
        """Loopback capture through the ``soundcard`` package (WASAPI on Windows)."""
        try:
            import soundcard
        except ImportError:
            raise SmartBulbError(
                "the soundcard capture backend needs the soundcard package (pip install soundcard)"
            ) from None
        np = self._np
        self._configure(self._rate, self._block, channels=2)
        loop = asyncio.get_running_loop()
        finished: asyncio.Future[None] = loop.create_future()
        stopping = threading.Event()

        def settle(error: Exception | None) -> None:
            if finished.done():
                return
            if error is None:
                finished.set_result(None)
            else:
                # some of soundcard's failures are bare assertions, so name the type too
                failure = SmartBulbError(f"sound capture failed: {type(error).__name__}: {error}".rstrip(": "))
                failure.__cause__ = error
                finished.set_exception(failure)

        def record() -> None:
            try:
                if self._mic:
                    chosen = self._device == DEFAULT_DEVICE
                    loopback = soundcard.default_microphone() if chosen else soundcard.get_microphone(id=self._device)
                else:
                    name = str(soundcard.default_speaker().name) if self._device == DEFAULT_DEVICE else self._device
                    loopback = soundcard.get_microphone(id=name, include_loopback=True)
                with loopback.recorder(samplerate=self._rate, channels=2, blocksize=self._block) as recorder:
                    while not stopping.is_set():
                        frames = recorder.record(numframes=self._block)
                        pair = frames[:, :2] if frames.shape[1] > 1 else np.repeat(frames, 2, axis=1)
                        pcm = (np.clip(pair, -1.0, 1.0) * 32767).astype("<i2").tobytes()
                        loop.call_soon_threadsafe(self.feed, pcm)
            except Exception as exc:
                loop.call_soon_threadsafe(settle, exc)
            else:
                loop.call_soon_threadsafe(settle, None)

        # A daemon thread: the recorder blocks while nothing is playing, and must not hold up exit.
        threading.Thread(target=record, name="chsmartbulb-capture", daemon=True).start()
        try:
            await finished
        finally:
            stopping.set()

    async def _capture_parec(self) -> None:
        self._configure(self._rate, self._block, channels=2)
        device = _DEFAULT_MICROPHONE if self._mic and self._device == DEFAULT_DEVICE else self._device
        command = [
            "parec",
            f"--device={device}",
            "--format=s16le",
            f"--rate={self._rate}",
            "--channels=2",
            "--raw",
            "--latency-msec=20",
        ]
        try:
            process = await asyncio.create_subprocess_exec(
                *command, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL
            )
        except FileNotFoundError:
            raise SmartBulbError("sound capture needs the 'parec' tool (pulseaudio-utils)") from None
        assert process.stdout is not None
        try:
            while True:
                data = await process.stdout.read(4096)
                if not data:
                    raise SmartBulbError("sound capture stopped unexpectedly")
                self.feed(data)
        finally:
            with contextlib.suppress(ProcessLookupError):
                process.kill()
            await process.wait()


def _set_delay(source: AudioSource, delay: float) -> None:
    if not 0.0 <= delay <= MAX_DELAY:
        raise ValueError(f"delay must be within 0..{MAX_DELAY:g} seconds")
    source.delay = delay


def _check_sensitivity(source: AudioSource, sensitivity: float) -> None:
    if not 0.0 <= sensitivity <= 1.0:
        raise ValueError("sensitivity must be within 0..1")
    source.sensitivity = sensitivity


def _loudness(levels: Levels) -> float:
    return max(levels.bass, levels.mid, levels.treble)


def music_pulse(
    source: AudioSource,
    color: Color | None = None,
    decay: float = 5.0,
    delay: float = 0.0,
    sensitivity: float = DEFAULT_SENSITIVITY,
) -> Effect:
    """Flash on every beat and glow with the bass in between.

    Without a ``color`` the hue steps to a new one on each beat. ``delay`` holds
    the light back to line up with a late audio output. ``sensitivity`` (0..1)
    sets how easily a rise in the bass counts as a beat.
    """
    _set_delay(source, delay)
    _check_sensitivity(source, sensitivity)

    def effect(t: float) -> Color:
        flash = math.exp(-decay * (source.clock() - source.last_beat))
        level = min(1.0, max(flash, 0.6 * source.levels.bass))
        if level < _DARK:
            return OFF  # scaled() never rounds a lit channel to zero, silence should be dark
        base = color if color is not None else Color.from_hsv(source.beats * 47.0)
        return base.scaled(level)

    return effect


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


def music_spectrum(source: AudioSource, release: float = 3.0, delay: float = 0.0) -> Effect:
    """Bass drives red, mids green and treble blue; ``release`` is the fall rate per second."""
    _set_delay(source, delay)
    shown = [0.0, 0.0, 0.0]
    last = 0.0

    def effect(t: float) -> Color:
        nonlocal last
        fall = release * max(0.0, t - last)
        last = t
        levels = source.levels
        for i, target in enumerate((levels.bass, levels.mid, levels.treble)):
            shown[i] = max(target, shown[i] - fall)
        # squared for contrast: quiet bands stay dim
        return Color(*(round(255 * value * value) for value in shown))

    return effect


def music_volume(
    source: AudioSource, color: Color = RED, release: float = 3.0, floor: float = 0.0, delay: float = 0.0
) -> Effect:
    """One colour whose brightness follows the loudness.

    ``release`` is the fall rate per second; ``floor`` (0..1) is the brightness kept in silence.
    """
    _set_delay(source, delay)
    if not 0.0 <= floor <= 1.0:
        raise ValueError("floor must be within 0..1")
    shown = 0.0
    last = 0.0

    def effect(t: float) -> Color:
        nonlocal shown, last
        shown = max(_loudness(source.levels), shown - release * max(0.0, t - last))
        last = t
        level = floor + (1.0 - floor) * shown * shown
        return color.scaled(level) if level >= _DARK else OFF

    return effect


def music_stereo(
    source: AudioSource,
    left: Color = BLUE,
    right: Color = RED,
    width: float = 4.0,
    release: float = 3.0,
    delay: float = 0.0,
) -> Effect:
    """Blend between two colours by where the sound sits; brightness follows the loudness.

    Sound on the left shows ``left``, on the right ``right``, and the middle is
    an even mix. Music rarely leans far to one side, so ``width`` stretches the
    measured position: at 4 a quarter of the way out already gives the pure colour.
    """
    _set_delay(source, delay)
    if width <= 0.0:
        raise ValueError("width must be positive")
    shown = 0.0
    position = 0.5
    last = 0.0

    def effect(t: float) -> Color:
        nonlocal shown, position, last
        step = max(0.0, t - last)
        last = t
        levels = source.levels
        loudness = _loudness(levels)
        faded = max(0.0, shown - release * step)
        if loudness > 0.0:  # silence says nothing about the position, keep the last one
            target = min(1.0, max(0.0, 0.5 + 0.5 * width * levels.balance))
            if faded * faded < _DARK:
                position = target  # out of the dark a sound starts where it is
            else:
                position += (target - position) * (1.0 - math.exp(-step / _PAN_SMOOTHING))
        shown = max(loudness, faded)
        level = shown * shown
        return left.mix(right, position).scaled(level) if level >= _DARK else OFF

    return effect
