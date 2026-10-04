"""The colour of what a screen shows, for the ``screen`` effect.

The bulb is one light, so the whole picture is reduced to a single colour.
Needs ``numpy`` and ``mss`` (``pip install chsmartbulb[screen]``); ``mss``
captures on Windows, macOS and X11, not on Wayland.

The capture can also run on another machine: :class:`RemoteScreen` is fed over
the network by ``chsmartbulb screen-agent``.
"""

from __future__ import annotations

import asyncio
import colorsys
import contextlib
import logging
import math
import threading
from typing import TYPE_CHECKING, Any, Protocol

from .color import OFF, Color
from .errors import SmartBulbError

if TYPE_CHECKING:
    from collections.abc import Callable

    from .effects import Effect

RATE = 15.0  # captures per second
PRIMARY = 1  # mss numbers the monitors from 1; 0 is all of them together

log = logging.getLogger(__name__)

_SAMPLES = 64  # pixels kept along the shorter side
_VIVID = 4.0  # how much more a fully saturated pixel counts than a grey one


class ScreenSource(Protocol):
    """What the screen effect reads and the service starts and stops."""

    color: Color

    async def start(self) -> None: ...

    async def stop(self) -> None: ...


class ScreenFeed(Protocol):
    """A running capture whose colours can be forwarded elsewhere."""

    on_color: Callable[[Color], None] | None

    async def start(self) -> None: ...

    async def wait(self) -> None: ...

    async def stop(self) -> None: ...


class RemoteScreen:
    """A screen whose colour arrives from elsewhere, for example over the network."""

    def __init__(self) -> None:
        self.color = OFF

    def push(self, color: Color) -> None:
        self.color = color

    def clear(self) -> None:
        """Go dark, for when the feed stops."""
        self.color = OFF

    async def start(self) -> None:
        """Nothing to start: the feed is not ours to control."""

    async def stop(self) -> None:
        """Nothing to stop."""


def picture_color(pixels: Any) -> Color:
    """Reduce RGB pixels (any shape ending in 3) to one colour.

    A plain average of a picture tends towards grey, so saturated pixels weigh more.
    """
    rgb = pixels.reshape(-1, 3).astype(float)
    weights = 1.0 + _VIVID * (rgb.max(axis=1) - rgb.min(axis=1)) / 255.0
    r, g, b = (rgb * weights[:, None]).sum(axis=0) / weights.sum()
    return Color(round(r), round(g), round(b))


class ScreenCapture:
    """Watches one monitor and keeps its colour in :attr:`color`.

    ``on_color`` is called whenever the colour changes, which is what the
    screen agent forwards.
    """

    def __init__(self, *, monitor: int = PRIMARY, rate: float = RATE) -> None:
        try:
            import mss
            import numpy
        except ImportError:
            raise SmartBulbError(
                "following the screen needs numpy and mss on the machine with the screen: "
                "pip install chsmartbulb[screen]"
            ) from None
        if rate <= 0:
            raise ValueError("rate must be positive")
        self._np: Any = numpy
        self._mss: Any = mss
        self._monitor = monitor
        self._interval = 1.0 / rate
        self._task: asyncio.Task[None] | None = None
        self._reported = False
        self.color = OFF
        self.on_color: Callable[[Color], None] | None = None

    async def start(self) -> None:
        """Begin watching the screen in the background."""
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

    def _show(self, color: Color) -> None:
        if color != self.color or not self._reported:  # the first one counts even when the screen is black
            self._reported = True
            self.color = color
            if self.on_color is not None:
                self.on_color(color)

    def _reduce(self, shot: Any) -> Color:
        frame = self._np.frombuffer(shot.raw, dtype=self._np.uint8).reshape(shot.height, shot.width, 4)
        step = max(1, min(shot.height, shot.width) // _SAMPLES)
        return picture_color(frame[::step, ::step, 2::-1])  # BGRA to RGB

    async def _capture(self) -> None:
        loop = asyncio.get_running_loop()
        finished: asyncio.Future[None] = loop.create_future()
        stopping = threading.Event()

        def settle(error: Exception | None) -> None:
            if finished.done():
                return
            if error is None:
                finished.set_result(None)
            else:
                failure = SmartBulbError(f"screen capture failed: {type(error).__name__}: {error}".rstrip(": "))
                failure.__cause__ = error
                finished.set_exception(failure)

        def watch() -> None:
            error: Exception | None = None
            try:
                # mss.mss is the spelling before 10.2, and an mss object must stay on the thread that made it
                with (getattr(self._mss, "MSS", None) or self._mss.mss)() as grabber:
                    monitors = grabber.monitors
                    if not 0 <= self._monitor < len(monitors):
                        raise LookupError(f"no monitor {self._monitor}; this machine has 1 to {len(monitors) - 1}")
                    area = monitors[self._monitor]
                    log.info("watching monitor %d (%dx%d)", self._monitor, area["width"], area["height"])
                    while not stopping.wait(self._interval):
                        color = self._reduce(grabber.grab(area))
                        loop.call_soon_threadsafe(self._show, color)
            except Exception as exc:
                error = exc
            with contextlib.suppress(RuntimeError):  # the loop is gone while shutting down
                loop.call_soon_threadsafe(settle, error)

        # A daemon thread: a grab can block and must not hold up exit.
        threading.Thread(target=watch, name="chsmartbulb-screen", daemon=True).start()
        try:
            await finished
            raise SmartBulbError("screen capture stopped unexpectedly")
        except SmartBulbError as exc:
            log.warning("%s", exc)
            raise
        finally:
            stopping.set()


def screen_follow(
    source: ScreenSource, smoothing: float = 0.2, saturation: float = 1.5, white: float = 1.0
) -> Effect:
    """Show the colour of the screen.

    ``smoothing`` is how many seconds the light takes to follow a change, which
    keeps cuts and scrolling from flickering. ``saturation`` multiplies the
    colourfulness: 1 leaves the screen's colour as it is, more makes it purer.
    ``white`` (0..1) is how much of the grey in the colour goes to the white
    LEDs: the bulb's red, green and blue together make a blue-violet, not a white.
    """
    if smoothing < 0.0:
        raise ValueError("smoothing must not be negative")
    if saturation < 0.0:
        raise ValueError("saturation must not be negative")
    if not 0.0 <= white <= 1.0:
        raise ValueError("white must be within 0..1")
    shown = [0.0, 0.0, 0.0]
    last = 0.0

    def effect(t: float) -> Color:
        nonlocal last
        step = max(0.0, t - last)
        last = t
        blend = 1.0 - math.exp(-step / smoothing) if smoothing else 1.0
        target = source.color
        for i, channel in enumerate((target.r, target.g, target.b)):
            shown[i] += (channel - shown[i]) * blend
        hue, colourfulness, value = colorsys.rgb_to_hsv(*(channel / 255.0 for channel in shown))
        return Color.from_hsv(hue * 360.0, min(1.0, colourfulness * saturation), value).with_white(white)

    return effect
