"""Sound-reactive effects driven by audio captured on the computer.

The sound is taken from the monitor of the default output, so it does not matter
where it plays: laptop speakers, headphones, another Bluetooth device or the bulb.
Needs ``numpy`` (``pip install chsmartbulb[audio]``) and the ``parec`` tool, which
ships with both PulseAudio and PipeWire.
"""

from __future__ import annotations

import asyncio
import contextlib
import math
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

_BANDS_HZ = ((40, 250), (250, 2000), (2000, 8000))
_GAIN_HALF_LIFE = 4.0  # seconds for the automatic gain to forget a loud passage
_SILENCE = 1e-4
_BEAT_RATIO = 1.5  # bass must exceed its recent average by this factor
_BEAT_GAP = 0.15  # seconds; caps detection at 400 bpm
_DARK = 1 / 255
_MAX_DELAY = 2.0


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


class MusicSource:
    """Analyses PCM audio into band levels and beats.

    :meth:`feed` does the analysis and can be driven by anything; :meth:`start`
    feeds it from the system's audio output.

    ``delay`` holds the results back by that many seconds. The capture hears the
    sound before a Bluetooth speaker or headphones play it, so without a delay
    the light runs ahead of what you hear.
    """

    def __init__(
        self,
        *,
        rate: int = RATE,
        block: int = BLOCK,
        device: str = DEFAULT_DEVICE,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        try:
            import numpy
        except ImportError:
            raise SmartBulbError("sound-reactive effects need numpy: pip install chsmartbulb[audio]") from None
        self._np: Any = numpy
        self._rate = rate
        self._block = block
        self._device = device
        self._clock = clock
        self._window = numpy.hanning(block)
        hz_per_bin = rate / block
        self._bins = [(max(1, round(lo / hz_per_bin)), round(hi / hz_per_bin)) for lo, hi in _BANDS_HZ]
        self._decay = 0.5 ** (block / rate / _GAIN_HALF_LIFE)
        self._peaks = [0.0, 0.0, 0.0]
        self._bass_average = 0.0
        self._pending = bytearray()
        self._task: asyncio.Task[None] | None = None
        self._last_onset = -math.inf
        self._held: deque[tuple[float, Levels, bool]] = deque()
        self.delay = 0.0
        self.levels = Levels()
        self.beats = 0
        self.last_beat = -math.inf

    def clock(self) -> float:
        """The time base :attr:`last_beat` is measured on."""
        return self._clock()

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

        self._held.append((now + self.delay, Levels(*levels), beat))
        while self._held and self._held[0][0] <= now:
            due, self.levels, was_beat = self._held.popleft()
            if was_beat:
                self.beats += 1
                self.last_beat = due

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

    async def _capture(self) -> None:
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
