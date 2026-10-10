"""Colour helpers, effect functions and the effect player."""

import asyncio
import itertools

import pytest

from chsmartbulb import BlockingLight, ChSmartBulb, Color, effects, parse_color
from chsmartbulb.cli import main
from chsmartbulb.errors import SmartBulbError
from fakes import FakeBulbTransport, RecordingLight, needs_native


def test_color_parsing_and_validation():
    assert Color.from_hex("#ff0064") == Color(255, 0, 100)
    assert Color.from_hex("00000080") == Color(w=128)
    assert parse_color("Red") == Color(r=255)
    assert Color(255, 0, 100, 7).to_hex() == "#ff006407"
    for bad in ("12345", "gggggg"):
        with pytest.raises(ValueError):
            Color.from_hex(bad)
    with pytest.raises(ValueError):
        Color(r=256)


def test_color_from_hsv_scaled_and_mix():
    assert Color.from_hsv(0) == Color(r=255)
    assert Color.from_hsv(120) == Color(g=255)
    assert Color.from_hsv(240) == Color(b=255)
    assert Color(200, 100, 0).scaled(0.5) == Color(100, 50, 0)
    assert Color(r=10).scaled(0.01) == Color(r=1)  # dim, not dark
    assert Color(r=10).scaled(0) == Color()
    assert Color(r=200).mix(Color(b=100), 0.5) == Color(r=100, b=50)


def test_play_sends_only_changed_frames_for_the_duration():
    def strobe(t):  # 5 Hz, half of each cycle on
        return Color(r=255) if (t * 5) % 1.0 < 0.5 else Color()

    async def scenario():
        light = RecordingLight()
        await effects.play(light, strobe, duration=0.7, fps=50)
        return light.colors

    colors = asyncio.run(scenario())
    assert colors[0] == Color(r=255)
    # 5 Hz for 0.7 s is 3.5 cycles = 7 on/off phases; sampled at 50 fps, duplicates are skipped.
    # The phases are long enough for a coarse clock: Windows ticks every 16 ms.
    assert 6 <= len(colors) <= 8
    assert all(a != b for a, b in itertools.pairwise(colors))


def test_player_replaces_and_stops_effects():
    async def scenario():
        light = RecordingLight()
        player = effects.EffectPlayer(light, fps=200)
        await player.start(lambda t: Color(r=255))
        await asyncio.sleep(0.03)
        await player.start(lambda t: Color(b=255))
        await asyncio.sleep(0.03)
        assert player.is_playing
        await player.stop()
        assert not player.is_playing
        return light.colors

    assert asyncio.run(scenario()) == [Color(r=255), Color(b=255)]


def test_effects_drive_a_real_bulb_object_through_its_brightness():
    async def scenario():
        transport = FakeBulbTransport()
        async with ChSmartBulb(transport) as bulb:
            await bulb.set_brightness(0.5)
            await effects.play(bulb, lambda t: Color(g=200), duration=0.05, fps=50)
        return transport.last_light

    assert asyncio.run(scenario())["g"] == 100


def test_blocking_wrapper_runs_coroutines_synchronously():
    transport = FakeBulbTransport()
    with BlockingLight(ChSmartBulb(transport)) as bulb:
        bulb.set_rgb(255, 0, 100)
        bulb.set_brightness(0.5)
        assert bulb.brightness == 0.5
        assert bulb.get_light_state().is_on
    assert (transport.last_light["r"], transport.last_light["b"]) == (128, 50)


def test_cli_requires_an_address_when_no_service_is_running(monkeypatch, capsys, tmp_path):
    monkeypatch.delenv("CHSMARTBULB_ADDRESS", raising=False)
    assert main(["--socket", str(tmp_path / "none.sock"), "on"]) == 1
    assert "no address given" in capsys.readouterr().err


def test_the_catalog_is_the_rust_one():
    needs_native()
    described = {info["name"]: info for info in effects.describe()}
    assert {"breathe", "custom", "music", "screen", "screensound"} <= set(described)
    assert described["music"]["needs"] == "audio"
    assert (described["screensound"]["needs"], described["screensound"]["also"]) == ("screen", "audio")

    breathe = effects.create("breathe", {"color": Color(g=255), "period": "4"})  # text, as the command line gives it
    assert (breathe(0), breathe(2)) == (Color(), Color(g=255))
    assert effects.create("hue", {"period": 6})(2) == Color(g=255)
    steps = [{"color": "#ff0000", "hold": 1.0}, {"color": "#0000ff", "hold": 1.0, "fade": 2.0}]
    assert effects.create("custom", {"steps": steps})(2.0) == Color(r=128, b=128)

    effects.check("strobe", {"hz": 2})
    for name, params, fragment in (
        ("sparkle", None, "unknown effect"),
        ("breathe", {"hz": 2}, "has no parameter"),
        ("breathe", {"period": "soon"}, "must be a number"),
        ("music", None, "no audio source"),
    ):
        with pytest.raises(ValueError, match=fragment):
            effects.create(name, params)
    with pytest.raises(TypeError, match="cannot be an effect parameter"):
        effects.create("breathe", {"period": object()})


def test_effects_follow_the_sources_they_are_given():
    needs_native()
    seen = effects.screen_source()
    screen = effects.create("screen", {"smoothing": 0, "saturation": 1, "white": 0}, screen=seen)
    seen.push(255, 0, 0)
    assert screen(0.1) == Color(r=255)
    seen.clear()
    assert screen(0.2) == Color()

    heard = effects.audio_source()
    volume = effects.create("volume", {"color": "00ff00"}, audio=heard)
    assert volume(0.0) == Color()
    heard.publish(0.0, 1.0, 0.0)
    assert volume(0.1) == Color(g=255)
    both = effects.create(
        "screensound", {"smoothing": 0, "saturation": 1, "white": 0, "floor": 0.2}, audio=heard, screen=seen
    )
    seen.push(255, 0, 0)
    assert both(0.0) == Color(r=255)
    heard.clear()
    assert both(60.0) == Color(r=51)  # silent: the floor


def test_the_catalog_says_what_it_needs_when_the_rust_core_is_missing(monkeypatch):
    monkeypatch.setenv("CHSMARTBULB_NATIVE", "0")
    with pytest.raises(SmartBulbError, match="chsmartbulb-native"):
        effects.describe()
    with pytest.raises(SmartBulbError, match="chsmartbulb-native"):
        effects.create("breathe")
