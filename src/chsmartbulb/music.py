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
import shutil
import sys
import threading
import time
from collections import deque
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Protocol

from .color import OFF, Color
from .errors import SmartBulbError

if TYPE_CHECKING:
    from collections.abc import Callable

    from .effects import Effect

RATE = 22050
BLOCK = 512  # 23 ms per analysis step
DEFAULT_DEVICE = "@DEFAULT_MONITOR@"
BACKENDS = ("auto", "parec", "wasapi", "soundcard")

log = logging.getLogger(__name__)

_BANDS_HZ = ((40, 250), (250, 2000), (2000, 8000))
_GAIN_HALF_LIFE = 4.0  # seconds for the automatic gain to forget a loud passage
_SILENCE = 1e-4
_BEAT_RATIO = 1.5  # bass must exceed its recent average by this factor
_BEAT_GAP = 0.15  # seconds; caps detection at 400 bpm
_DARK = 1 / 255
_MAX_DELAY = 2.0
_STREAM_POLL = 0.5  # seconds between checks that a callback-driven stream is still alive


@dataclass(frozen=True)
class Levels:
    """Band loudness relative to the recent peak, each 0..1."""

    bass: float = 0.0
    mid: float = 0.0
    treble: float = 0.0


class AudioSource(Protocol):
    """What the sound-reactive effects read and the service starts and stops."""

    levels: Levels
    beats: int
    last_beat: float
    delay: float

    def clock(self) -> float: ...

    async def start(self) -> None: ...

    async def stop(self) -> None: ...


class Capture(Protocol):
    """A running analysis whose blocks can be forwarded elsewhere."""

    on_block: Callable[[Levels, bool], None] | None

    async def start(self) -> None: ...

    async def wait(self) -> None: ...

    async def stop(self) -> None: ...


class _Published:
    """Levels and beats as the effects see them.

    ``delay`` holds them back by that many seconds. The capture hears the sound
    before a Bluetooth speaker or headphones play it, so without a delay the
    light runs ahead of what you hear.
    """

    def __init__(self, clock: Callable[[], float]) -> None:
        self._clock = clock
        self._held: deque[tuple[float, Levels, bool]] = deque()
        self.delay = 0.0
        self.levels = Levels()
        self.beats = 0
        self.last_beat = -math.inf

    def clock(self) -> float:
        """The time base :attr:`last_beat` is measured on."""
        return self._clock()

    def _publish(self, levels: Levels, beat: bool) -> None:
        now = self.clock()
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

    def push(self, levels: Levels, beat: bool) -> None:
        self._publish(levels, beat)

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
    feeds it from the system's audio output. ``on_block`` is called with every
    analysed block before any delay, which is what the audio agent forwards.
    """

    def __init__(
        self,
        *,
        rate: int = RATE,
        block: int = BLOCK,
        device: str = DEFAULT_DEVICE,
        backend: str = "auto",
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        super().__init__(clock)
        try:
            import numpy
        except ImportError:
            raise SmartBulbError("sound-reactive effects need numpy: pip install chsmartbulb[audio]") from None
        if backend not in BACKENDS:
            raise ValueError(f"unknown audio backend {backend!r}; choose from: {', '.join(BACKENDS)}")
        self._np: Any = numpy
        self._device = device
        self._backend = backend
        self.on_block: Callable[[Levels, bool], None] | None = None
        self._configure(rate, block)
        self._peaks = [0.0, 0.0, 0.0]
        self._bass_average = 0.0
        self._task: asyncio.Task[None] | None = None
        self._last_onset = -math.inf

    def _configure(self, rate: int, block: int | None = None) -> None:
        """Size the analysis for ``rate``; without ``block``, pick one of about 23 ms."""
        if block is None:
            block = BLOCK
            while rate / block > 60:  # keep the bins near 45 Hz wide so the bass band stays resolved
                block *= 2
        self._rate = rate
        self._block = block
        self._window = self._np.hanning(block)
        hz_per_bin = rate / block
        self._bins = [(max(1, round(lo / hz_per_bin)), round(hi / hz_per_bin)) for lo, hi in _BANDS_HZ]
        self._decay = 0.5 ** (block / rate / _GAIN_HALF_LIFE)
        self._pending = bytearray()

    def feed(self, pcm: bytes) -> None:
        """Consume signed 16-bit little-endian mono samples."""
        self._pending += pcm
        size = self._block * 2
        while len(self._pending) >= size:
            samples = self._np.frombuffer(bytes(self._pending[:size]), dtype="<i2") / 32768.0
            del self._pending[:size]
            self._analyse(samples)

    def _analyse(self, samples: Any) -> None:
        spectrum = self._np.abs(self._np.fft.rfft(samples * self._window)) / (self._block / 2)
        values = [float(spectrum[lo:hi].mean()) for lo, hi in self._bins]
        levels = []
        for i, value in enumerate(values):
            self._peaks[i] = max(value, self._peaks[i] * self._decay)
            levels.append(value / self._peaks[i] if self._peaks[i] > _SILENCE else 0.0)

        bass = values[0]
        now = self.clock()
        beat = bass > _SILENCE and bass > _BEAT_RATIO * self._bass_average and now - self._last_onset > _BEAT_GAP
        if beat:
            self._last_onset = now
        # fast enough to catch up with a sustained note before the gap allows another beat
        self._bass_average += 0.25 * (bass - self._bass_average)

        result = Levels(*levels)
        if self.on_block is not None:
            self.on_block(result, beat)
        self._publish(result, beat)

    async def start(self) -> None:
        """Begin capturing the system's audio output in the background."""
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
        np = self._np
        loop = asyncio.get_running_loop()
        audio = pyaudio.PyAudio()
        try:
            device = self._wasapi_loopback(audio)
            channels = int(device["maxInputChannels"])
            rate = int(device["defaultSampleRate"])
            self._configure(rate)

            def on_audio(data: bytes, _frames: int, _time: Any, _status: int) -> tuple[None, int]:
                samples = np.frombuffer(data, dtype="<i2")
                if channels > 1:
                    samples = samples.reshape(-1, channels).mean(axis=1).astype("<i2")
                with contextlib.suppress(RuntimeError):  # the loop is gone while shutting down
                    loop.call_soon_threadsafe(self.feed, samples.tobytes())
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
                while stream.is_active():  # noqa: ASYNC110 - PortAudio offers nothing to await
                    await asyncio.sleep(_STREAM_POLL)
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

    async def _capture_soundcard(self) -> None:
        """Loopback capture through the ``soundcard`` package (WASAPI on Windows)."""
        try:
            import soundcard
        except ImportError:
            raise SmartBulbError(
                "the soundcard capture backend needs the soundcard package (pip install soundcard)"
            ) from None
        np = self._np
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
                name = str(soundcard.default_speaker().name) if self._device == DEFAULT_DEVICE else self._device
                loopback = soundcard.get_microphone(id=name, include_loopback=True)
                with loopback.recorder(samplerate=self._rate, channels=1, blocksize=self._block) as recorder:
                    while not stopping.is_set():
                        frames = recorder.record(numframes=self._block)
                        pcm = (np.clip(frames[:, 0], -1.0, 1.0) * 32767).astype("<i2").tobytes()
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
        command = [
            "parec",
            f"--device={self._device}",
            "--format=s16le",
            f"--rate={self._rate}",
            "--channels=1",
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
    if not 0.0 <= delay <= _MAX_DELAY:
        raise ValueError(f"delay must be within 0..{_MAX_DELAY:g} seconds")
    source.delay = delay


def music_pulse(source: AudioSource, color: Color | None = None, decay: float = 5.0, delay: float = 0.0) -> Effect:
    """Flash on every beat and glow with the bass in between.

    Without a ``color`` the hue steps to a new one on each beat. ``delay`` holds
    the light back to line up with a late audio output.
    """
    _set_delay(source, delay)

    def effect(t: float) -> Color:
        flash = math.exp(-decay * (source.clock() - source.last_beat))
        level = min(1.0, max(flash, 0.6 * source.levels.bass))
        if level < _DARK:
            return OFF  # scaled() never rounds a lit channel to zero, silence should be dark
        base = color if color is not None else Color.from_hsv(source.beats * 47.0)
        return base.scaled(level)

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
