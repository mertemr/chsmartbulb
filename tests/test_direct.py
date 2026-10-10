"""Requests carried out straight on the bulb, as the command line does without a service."""

import asyncio

import pytest

from chsmartbulb import ChSmartBulb, Color, cli, direct
from chsmartbulb import protocol as p
from chsmartbulb.errors import SmartBulbError
from chsmartbulb.music import Levels
from chsmartbulb.protocol import NativeEffect
from fakes import FakeBulbTransport, ScriptedScreen, ScriptedSource, needs_native


def run(coroutine):
    return asyncio.run(coroutine)


def ask(transport, request, **options):
    return run(direct.run(ChSmartBulb(transport), request, **options))


def test_light_commands_reach_the_bulb():
    transport = FakeBulbTransport()
    assert ask(transport, {"cmd": "color", "color": "#ff8000", "brightness": 0.5, "fade": True}) == {}
    assert transport.last_light == dict(r=128, g=64, b=0, w=0, y=0, speed=0, effect=NativeEffect.FIXED, fade=1)
    ask(transport, {"cmd": "off"})
    assert transport.channels == [0, 0, 0, 0, 0]
    ask(transport, {"cmd": "color", "color": "#000000"})  # black is off
    assert transport.channels == [0, 0, 0, 0, 0]
    ask(transport, {"cmd": "on"})
    assert max(transport.channels) == 255
    with pytest.raises(ValueError, match="unknown command"):
        ask(transport, {"cmd": "dance"})
    with pytest.raises(ValueError, match="brightness"):
        ask(transport, {"cmd": "color", "color": "#ff0000", "brightness": 3})


def test_brightness_dims_the_colour_the_bulb_reports():
    transport = FakeBulbTransport()
    ask(transport, {"cmd": "color", "color": "#ff8000"})
    ask(transport, {"cmd": "brightness", "level": 0.5})  # a new process: the colour comes from the bulb
    assert (transport.last_light["r"], transport.last_light["g"]) == (128, 64)
    ask(transport, {"cmd": "brightness", "level": 1.0})  # and it reports the mix, however dim it is
    assert (transport.last_light["r"], transport.last_light["g"]) == (255, 127)
    assert ask(transport, {"cmd": "status"}) == {"connected": True, "bulb": "#ff7f00"}
    ask(transport, {"cmd": "off"})
    with pytest.raises(SmartBulbError, match="the light is off"):
        ask(transport, {"cmd": "brightness", "level": 0.5})


def test_native_effects_and_stopping_them():
    transport = FakeBulbTransport()
    ask(transport, {"cmd": "native", "name": "breathing", "color": "#0000ff", "speed": 3, "brightness": 0.5})
    assert (transport.last_light["b"], transport.last_light["effect"]) == (128, NativeEffect.BREATHING)
    ask(transport, {"cmd": "stop"})
    assert (transport.last_light["b"], transport.last_light["effect"]) == (255, NativeEffect.FIXED)
    ask(transport, {"cmd": "native", "name": "breathing"})  # no colour given: the one showing
    assert (transport.last_light["b"], transport.last_light["effect"]) == (255, NativeEffect.BREATHING)
    for name in ("fixed", "sparkle"):
        with pytest.raises(ValueError, match="unknown native effect"):
            ask(transport, {"cmd": "native", "name": name})


def test_device_queries():
    transport = FakeBulbTransport()
    info = ask(transport, {"cmd": "info"})
    assert (info["name"], info["model"], info["model_id"]) == ("SmartBulb Bluetooth", "BL04", 0x0C47)
    timers = ask(transport, {"cmd": "timers"})["timers"]
    assert [(timer["index"], timer["name"], timer["enabled"]) for timer in timers] == [
        (6, "power off", True),
        (5, "power on", False),
    ]
    assert ask(transport, {"cmd": "timer", "index": 5, "enabled": True})["timer"]["enabled"] is True
    identify = p.query(p.Command.IDENTIFY).encode().hex()
    assert ask(transport, {"cmd": "raw", "hex": identify}) == {"answer": "01fe000041021000470c000000000000"}
    assert ask(transport, {"cmd": "raw", "hex": p.light(Color(g=9)).encode().hex()}) == {}
    assert transport.last_light["g"] == 9


def test_effects_of_the_catalog_play_here():
    needs_native()
    transport = FakeBulbTransport()
    request = {"cmd": "effect", "name": "strobe", "params": {"color": "#00ff00", "hz": 5}, "duration": 0.3}
    assert ask(transport, {**request, "brightness": 0.5}, fps=100) == {}
    greens = {body[0] for body in transport.light_bodies}
    assert greens == {0, 128}
    with pytest.raises(ValueError, match="unknown effect"):
        ask(transport, {"cmd": "effect", "name": "sparkle"})


def test_effects_follow_the_sound_and_the_screen_captured_here():
    needs_native()
    transport = FakeBulbTransport()
    loud = ScriptedSource([(Levels(mid=1.0), 0.0)] * 40)
    request = {"cmd": "effect", "name": "volume", "params": {"color": "#00ff00"}, "duration": 0.15}
    ask(transport, request, fps=100, music=lambda: loud)
    assert max(body[0] for body in transport.light_bodies) == 255
    assert loud._task is None  # the capture was stopped with the effect

    transport = FakeBulbTransport()
    red = ScriptedScreen([Color(r=255)])
    request = {"cmd": "effect", "name": "screen", "params": {"smoothing": 0, "white": 0}, "duration": 0.15}
    ask(transport, request, fps=100, screen=lambda: red)
    assert transport.last_light["r"] == 255

    transport = FakeBulbTransport()
    request = {"cmd": "effect", "name": "screensound", "params": {"smoothing": 0, "white": 0}, "duration": 0.15}
    ask(transport, request, fps=100, music=lambda: loud, screen=lambda: red)
    assert transport.last_light["r"] > 0
    with pytest.raises(SmartBulbError, match="follows the sound"):
        ask(transport, request, screen=lambda: red)
    with pytest.raises(SmartBulbError, match="follows the screen"):
        ask(transport, request, music=lambda: loud)


def test_a_capture_that_fails_ends_the_effect_with_its_error():
    needs_native()

    class Broken(ScriptedSource):
        async def wait(self):
            raise SmartBulbError("sound capture stopped unexpectedly")

    with pytest.raises(SmartBulbError, match="stopped unexpectedly"):
        ask(FakeBulbTransport(), {"cmd": "effect", "name": "music"}, music=lambda: Broken([]))


def test_the_cli_goes_to_the_bulb_when_no_service_runs(monkeypatch, capsys, tmp_path):
    transport = FakeBulbTransport()
    monkeypatch.setattr(cli, "_bulb", lambda args, **options: ChSmartBulb(transport))
    base = ["--socket", str(tmp_path / "none.sock")]
    assert cli.main([*base, "rgb", "0", "200", "0", "-b", "50"]) == 0
    assert transport.last_light["g"] == 100
    assert cli.main([*base, "status"]) == 0
    assert capsys.readouterr().out == (
        "bulb:       connected\nreported:   #00ff00 (colour mix; the bulb does not report brightness)\n"
    )
    assert cli.main([*base, "timers"]) == 0
    assert "#6 'power off' 06:20" in capsys.readouterr().out


def test_the_cli_lists_and_checks_effects_with_the_rust_core(monkeypatch, capsys, tmp_path):
    base = ["--socket", str(tmp_path / "none.sock")]
    monkeypatch.setenv("CHSMARTBULB_NATIVE", "0")
    assert cli.main([*base, "effects"]) == 1
    assert "chsmartbulb-native" in capsys.readouterr().err
    assert cli.main([*base, "-a", "AA:BB:CC:DD:EE:FF", "effect", "breathe", "-d", "0.1"]) == 1
    assert "chsmartbulb-native" in capsys.readouterr().err

    monkeypatch.delenv("CHSMARTBULB_NATIVE")
    needs_native()
    assert cli.main([*base, "effects"]) == 0
    out = capsys.readouterr().out
    assert "breathe     swell and fade" in out
    assert "[screen, audio]" in out
    assert cli.main([*base, "effect", "breathe", "-s", "hz=3"]) == 1  # refused before anything is connected
    assert "has no parameter" in capsys.readouterr().err
    transport = FakeBulbTransport()
    monkeypatch.setattr(cli, "_bulb", lambda args, **options: ChSmartBulb(transport))
    assert cli.main([*base, "effect", "hue", "-p", "1", "-d", "0.1", "--fps", "100"]) == 0
    assert len(transport.light_bodies) > 3
