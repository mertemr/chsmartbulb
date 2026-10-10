"""The colour of what a screen shows.

The bulb is one light, so the whole picture is reduced to a single colour.
Needs ``numpy`` and ``mss`` (``pip install chsmartbulb[screen]``); ``mss``
captures on Windows, macOS and X11, not on Wayland.

This is what ``chsmartbulb screen-agent`` forwards to a service, and what a
screen effect played by this process follows.
"""

from __future__ import annotations

import asyncio
import contextlib
import logging
import threading
from typing import TYPE_CHECKING, Any, Protocol

from .color import OFF, Color
from .errors import SmartBulbError

if TYPE_CHECKING:
    from collections.abc import Callable

RATE = 15.0  # captures per second
PRIMARY = 1  # mss numbers the monitors from 1; 0 is all of them together
STILL_FRAMES = 15  # frames without a change before the capture starts to slow down (about a second)
SLOWEST = 5  # at most this many frame times between two captures of a picture that stands still

log = logging.getLogger(__name__)

_SAMPLES = 64  # pixels kept along the shorter side
_VIVID = 4.0  # how much more a fully saturated pixel counts than a grey one


def pace(still: int) -> int:
    """How many frame times to wait before the next capture, after ``still`` unchanged frames."""
    return min(1 + still // STILL_FRAMES, SLOWEST)


def similar(a: Color, b: Color, tolerance: int = 2) -> bool:
    """Whether two colours differ by no more than the noise of a compressed picture."""
    return max(abs(a.r - b.r), abs(a.g - b.g), abs(a.b - b.b)) <= tolerance


def list_monitors() -> list[dict[str, int]]:
    """The monitors of this machine as ``{"index", "width", "height"}`` (``[]`` where nothing can capture)."""
    try:
        import mss
    except ImportError:
        return []
    try:
        with (getattr(mss, "MSS", None) or mss.mss)() as grabber:
            return [
                {"index": index, "width": area["width"], "height": area["height"]}
                for index, area in enumerate(grabber.monitors)
                if index > 0  # 0 is all of them together, which the interface offers by itself
            ]
    except Exception:  # no display, no permission: just no list
        log.debug("cannot list the monitors", exc_info=True)
        return []


class ScreenFeed(Protocol):
    """A running capture whose colours can be forwarded elsewhere."""

    on_color: Callable[[Color], None] | None

    async def start(self) -> None: ...

    async def wait(self) -> None: ...

    async def stop(self) -> None: ...


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
                    still, last = 0, None
                    while not stopping.wait(self._interval * pace(still)):
                        color = self._reduce(grabber.grab(area))
                        still = still + 1 if last is not None and similar(color, last) else 0
                        last = color
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
