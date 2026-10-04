"""The screen effect: reducing a picture to one colour, capture, and the effect itself."""

import asyncio
import sys
import types

import pytest

from chsmartbulb import Color, catalog, screen
from chsmartbulb.errors import SmartBulbError
from chsmartbulb.screen import RemoteScreen
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


def test_screen_effect_follows_smoothly_and_boosts_the_colour():
    source = RemoteScreen()
    follow = catalog.create("screen", {"smoothing": 0.5, "saturation": 1}, screen=source)
    assert follow(0.0) == Color()
    source.push(Color(r=200, g=100))
    part = follow(0.1)
    assert 0 < part.r < 60  # on its way
    assert follow(10.0) == Color(r=200, g=100)
    source.clear()
    assert follow(20.0) == Color()

    source.push(Color(r=200, g=100, b=100))
    vivid = catalog.create("screen", {"smoothing": 0, "saturation": 2}, screen=source)
    assert vivid(0.0) == Color(r=200)  # twice as colourful: the grey part is gone
    with pytest.raises(ValueError, match="no screen source"):
        catalog.create("screen")
    with pytest.raises(ValueError, match="smoothing"):
        catalog.create("screen", {"smoothing": -1}, screen=source)
