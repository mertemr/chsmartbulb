"""Host-driven effects: colours computed on the computer and streamed to any :class:`Light`.

An effect is just a function from elapsed seconds to a :class:`Color`. :func:`play`
samples one at a fixed rate and sends the frames; :class:`EffectPlayer` runs it in
the background. Your own function works as well as one from the catalog.

The catalog itself lives in the Rust core, which the services run. :func:`create`
builds its effects here through ``chsmartbulb-native``, so there is one of each.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import time
from collections.abc import Callable, Mapping
from typing import TYPE_CHECKING, Any

from . import _native
from .color import Color

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


# --- the catalog ----------------------------------------------------------------------


def _text(params: Mapping[str, Any] | None) -> str | None:
    def plain(value: Any) -> Any:
        if isinstance(value, Color):
            return value.to_hex()
        raise TypeError(f"{value!r} cannot be an effect parameter")

    return None if params is None else json.dumps(params, default=plain)


def describe() -> list[dict[str, Any]]:
    """The catalog as plain data: each effect's name, summary, parameters and what it follows."""
    return json.loads(_native.require("the effect catalog").describe())


def check(name: str, params: Mapping[str, Any] | None = None) -> None:
    """Raise :class:`ValueError` unless ``params`` fit the effect ``name``."""
    _native.require("the effect catalog").check(name, _text(params))


def audio_source() -> Any:
    """What a sound-reactive effect follows: ``publish(bass, mid, treble, balance, onset)`` each block into it."""
    return _native.require("a sound-reactive effect").AudioSource()


def screen_source() -> Any:
    """What a screen effect follows: ``push(r, g, b)`` the colour of the picture into it."""
    return _native.require("a screen effect").ScreenSource()


def create(
    name: str,
    params: Mapping[str, Any] | None = None,
    *,
    audio: Any = None,
    screen: Any = None,
) -> Effect:
    """Build the named effect of the catalog.

    Those that follow the sound or the screen need an :func:`audio_source` or a
    :func:`screen_source`, fed by whatever captures.
    """
    built = _native.require("the effect catalog").create(name, _text(params), audio, screen)
    return lambda t: Color(*built(t))
