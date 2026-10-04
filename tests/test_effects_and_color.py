"""Colour helpers, effect functions and the effect player."""

import asyncio
import itertools

import pytest

from chsmartbulb import BlockingLight, ChSmartBulb, Color, effects, parse_color
from chsmartbulb.cli import main
from fakes import FakeBulbTransport, RecordingLight


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


def test_effect_functions():
    red, blue = Color(r=255), Color(b=255)
    breathe = effects.breathe(red, period=4)
    assert breathe(0) == Color()
    assert breathe(2) == red
    assert 0 < breathe(1).r < 255
    assert effects.hue_cycle(period=6)(2) == Color(g=255)
    strobe = effects.strobe(red, hz=2)
    assert (strobe(0.1), strobe(0.3)) == (red, Color())
    assert effects.pulse(red, period=1)(0) == red
    assert effects.pulse(red, period=1)(0.9).r < 20
    assert effects.fade(red, blue, 2)(1) == Color(r=128, b=128)
    assert effects.fade(red, blue, 2)(5) == blue
    assert effects.dimmed(effects.solid(red), 0.5)(0) == Color(r=128)


def test_sequence_holds_fades_and_loops():
    red, blue = Color(r=255), Color(b=255)
    seq = effects.sequence([(red, 1.0), (blue, 1.0, 1.0)])  # red 1 s, fade 1 s, blue 1 s
    assert seq(0.5) == red
    assert seq(1.5) == Color(r=128, b=128)
    assert seq(2.5) == blue
    assert seq(3.5) == red  # looped
    once = effects.sequence([(red, 1.0), (blue, 1.0)], loop=False)
    assert once(99) == blue
    with pytest.raises(ValueError):
        effects.sequence([])


def test_play_sends_only_changed_frames_for_the_duration():
    async def scenario():
        light = RecordingLight()
        await effects.play(light, effects.strobe(Color(r=255), hz=5), duration=0.7, fps=50)
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
        await player.start(effects.solid(Color(r=255)))
        await asyncio.sleep(0.03)
        await player.start(effects.solid(Color(b=255)))
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
            await effects.play(bulb, effects.solid(Color(g=200)), duration=0.05, fps=50)
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
