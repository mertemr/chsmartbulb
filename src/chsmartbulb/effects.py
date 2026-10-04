"""Host-driven effects: colours computed on the computer and streamed to any :class:`Light`.

An effect is just a function from elapsed seconds to a :class:`Color`. That keeps
effects pure and testable, and lets them be combined freely. :func:`play` samples
one at a fixed rate and sends the frames; :class:`EffectPlayer` runs it in the
background.
"""

from __future__ import annotations

import asyncio
import contextlib
import math
import time
from collections.abc import Callable, Sequence
from typing import TYPE_CHECKING

from .color import OFF, Color

if TYPE_CHECKING:
    from .light import Light

Effect = Callable[[float], Color]

#: The CHSmartBulb follows about 25 colour changes per second; 20 leaves headroom.
DEFAULT_FPS = 20.0


async def play(
    light: Light,
    effect: Effect,
    *,
    duration: float | None = None,
    fps: float = DEFAULT_FPS,
) -> None:
    """Stream ``effect`` to ``light`` until ``duration`` elapses (forever when ``None``)."""
    if fps <= 0:
        raise ValueError("fps must be positive")
    interval = 1.0 / fps
    start = time.monotonic()
    last: Color | None = None
    frame = 0
    while True:
        elapsed = time.monotonic() - start
        if duration is not None and elapsed >= duration:
            return
        color = effect(elapsed)
        if color != last:
            await light.set_color(color)
            last = color
        frame += 1
        # schedule against the start time so slow sends do not accumulate drift
        await asyncio.sleep(max(0.0, start + frame * interval - time.monotonic()))


class EffectPlayer:
    """Runs one effect at a time in a background task."""

    def __init__(self, light: Light, *, fps: float = DEFAULT_FPS) -> None:
        self._light = light
        self._fps = fps
        self._task: asyncio.Task | None = None

    @property
    def is_playing(self) -> bool:
        return self._task is not None and not self._task.done()

    async def start(self, effect: Effect, *, duration: float | None = None) -> None:
        """Replace whatever is playing with ``effect``."""
        await self.stop()
        self._task = asyncio.create_task(play(self._light, effect, duration=duration, fps=self._fps))

    async def stop(self) -> None:
        task, self._task = self._task, None
        if task is not None:
            task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await task

    async def wait(self) -> None:
        """Wait for a finite effect to finish; re-raises its error if it failed."""
        if self._task is not None:
            await self._task


# --- building blocks ------------------------------------------------------------------


def solid(color: Color) -> Effect:
    return lambda t: color


def breathe(color: Color, period: float = 4.0, floor: float = 0.0) -> Effect:
    """Swell between ``floor`` (0..1) and full brightness, starting dark."""

    def effect(t: float) -> Color:
        level = (1 - math.cos(2 * math.pi * t / period)) / 2
        return color.scaled(floor + (1 - floor) * level * level)  # squared: gentler at the dim end

    return effect


def hue_cycle(period: float = 10.0, saturation: float = 1.0, value: float = 1.0) -> Effect:
    """Walk once around the colour wheel every ``period`` seconds."""
    return lambda t: Color.from_hsv(360 * t / period, saturation, value)


def pulse(color: Color, period: float = 1.0, decay: float = 4.0, base: Color = OFF) -> Effect:
    """Flash to ``color`` at the start of every period, then decay towards ``base``."""

    def effect(t: float) -> Color:
        phase = (t % period) / period
        return base.mix(color, math.exp(-decay * phase))

    return effect


def strobe(color: Color, hz: float = 5.0, duty: float = 0.5, base: Color = OFF) -> Effect:
    """Hard on/off blinking."""
    return lambda t: color if (t * hz) % 1.0 < duty else base


def fade(start: Color, end: Color, duration: float) -> Effect:
    """Blend from ``start`` to ``end`` over ``duration`` seconds, then hold ``end``."""
    return lambda t: start.mix(end, t / duration if duration > 0 else 1.0)


def sequence(
    steps: Sequence[tuple[Color, float] | tuple[Color, float, float]],
    *,
    loop: bool = True,
) -> Effect:
    """A custom colour sequence with custom timing.

    Each step is ``(color, hold)`` or ``(color, hold, fade_in)``: the light blends
    from the previous step's colour over ``fade_in`` seconds, then holds for
    ``hold`` seconds.
    """
    if not steps:
        raise ValueError("sequence needs at least one step")
    normalized = [(s[0], float(s[1]), float(s[2]) if len(s) > 2 else 0.0) for s in steps]
    total = sum(hold + fade_in for _, hold, fade_in in normalized)
    if total <= 0:
        raise ValueError("sequence must last longer than zero seconds")

    def effect(t: float) -> Color:
        if loop:
            t %= total
        elif t >= total:
            return normalized[-1][0]
        # before the first lap completes there is no previous colour to blend from
        previous = normalized[-1][0] if loop else normalized[0][0]
        for color, hold, fade_in in normalized:
            if t < fade_in:
                return previous.mix(color, t / fade_in)
            t -= fade_in
            if t < hold:
                return color
            t -= hold
            previous = color
        return normalized[-1][0]

    return effect


def dimmed(effect: Effect, level: float) -> Effect:
    """Scale another effect's output."""
    return lambda t: effect(t).scaled(level)
