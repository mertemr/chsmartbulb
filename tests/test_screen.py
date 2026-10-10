"""The screen effect: reducing a picture to one colour, capture, and the effect itself."""

import asyncio
import sys
import types

import pytest

from chsmartbulb import Color, screen
from chsmartbulb.errors import SmartBulbError
from fakes import until

np = pytest.importorskip("numpy")


def fake_mss(shots, *, spelling="MSS"):
    """An ``mss`` module whose two monitors show ``shots`` (BGR), the last one for ever."""

    class Grabber:
        monitors = (
            {"left": 0, "top": 0, "width": 300, "height": 100},
            {"left": 0, "top": 0, "width": 200, "height": 100},
        )

        def __enter__(self):
            return self

        def __exit__(self, *exc):
            return False

        def grab(self, area):
            pixel = shots.pop(0) if len(shots) > 1 else shots[0]
            raw = bytearray([*pixel, 255] * (area["width"] * area["height"]))
            return types.SimpleNamespace(raw=raw, width=area["width"], height=area["height"])

    return types.SimpleNamespace(**{spelling: Grabber})


def test_saturated_pixels_weigh_more_than_grey_ones():
    assert screen.picture_color(np.full((4, 4, 3), 90, dtype=np.uint8)) == Color(90, 90, 90)
    picture = np.full((10, 10, 3), 128, dtype=np.uint8)
    picture[:2] = (255, 0, 0)  # a fifth of it is pure red
    color = screen.picture_color(picture)
    assert color.r > 190  # a plain average would give 153
    assert color.g == color.b < 70


def test_capture_reports_the_monitor_colour_when_it_changes(monkeypatch):
    red, blue = (0, 0, 255), (255, 0, 0)
    monkeypatch.setitem(sys.modules, "mss", fake_mss([red, red, red, blue]))

    async def scenario():
        capture = screen.ScreenCapture(rate=500)
        seen = []
        capture.on_color = seen.append
        await capture.start()
        await until(lambda: len(seen) == 2)
        assert capture.color == Color(b=255)
        await capture.stop()
        return seen

    assert asyncio.run(scenario()) == [Color(r=255), Color(b=255)]


def test_capture_reports_a_black_screen_once_and_names_a_missing_monitor(monkeypatch):
    monkeypatch.setitem(sys.modules, "mss", fake_mss([(0, 0, 0)], spelling="mss"))  # how mss was opened before 10.2

    async def scenario():
        capture = screen.ScreenCapture(rate=500)
        seen = []
        capture.on_color = seen.append
        await capture.start()
        await until(lambda: bool(seen))
        await asyncio.sleep(0.02)
        await capture.stop()
        assert seen == [Color()]

        missing = screen.ScreenCapture(monitor=4, rate=500)
        await missing.start()
        with pytest.raises(SmartBulbError, match="screen capture failed: LookupError: no monitor 4"):
            await missing.wait()

    asyncio.run(scenario())


def test_missing_capture_library_is_reported_clearly(monkeypatch):
    monkeypatch.setitem(sys.modules, "mss", None)
    with pytest.raises(SmartBulbError, match=r"pip install chsmartbulb\[screen\]"):
        screen.ScreenCapture()


def test_capture_slows_down_while_the_picture_stands_still():
    assert screen.pace(0) == 1
    assert screen.pace(screen.STILL_FRAMES - 1) == 1
    assert screen.pace(screen.STILL_FRAMES) == 2
    assert screen.pace(10_000) == screen.SLOWEST  # never slower than that, however long it stands
    assert screen.similar(Color(100, 100, 100), Color(101, 99, 102))  # compression noise is not movement
    assert not screen.similar(Color(100, 100, 100), Color(100, 100, 110))
