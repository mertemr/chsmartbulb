"""The background service: request handling, remembered state, recovery and the socket."""

import asyncio
import json

import pytest

from chsmartbulb import ChSmartBulb, client, service
from chsmartbulb.cli import main
from chsmartbulb.errors import SmartBulbError
from fakes import FakeBulbTransport, FakeMusic

unix_sockets = pytest.mark.skipif(not hasattr(asyncio, "start_unix_server"), reason="needs Unix sockets")


def run(coroutine):
    return asyncio.run(coroutine)


async def attached(transport, **options):
    daemon = service.BulbService(ChSmartBulb(transport), music_factory=FakeMusic, **options)
    await daemon.attach()
    return daemon


async def until(condition, *, limit=1.0):
    """Give background tasks time to reach ``condition``."""
    deadline = asyncio.get_running_loop().time() + limit
    while not condition():
        assert asyncio.get_running_loop().time() < deadline, "condition was never met"
        await asyncio.sleep(0.005)


def test_colour_brightness_and_power_requests_drive_the_bulb():
    async def scenario():
        transport = FakeBulbTransport()
        daemon = await attached(transport)
        assert (await daemon.handle({"cmd": "color", "color": "#ff0064", "brightness": 0.5}))["ok"]
        assert (transport.last_light["r"], transport.last_light["b"]) == (128, 50)
        await daemon.handle({"cmd": "off"})
        assert transport.channels == [0, 0, 0, 0, 0]
        await daemon.handle({"cmd": "brightness", "level": 1.0})
        assert transport.channels == [0, 0, 0, 0, 0]  # dimming must not switch the light on
        await daemon.handle({"cmd": "on", "fade": True})
        assert (transport.last_light["r"], transport.last_light["fade"]) == (255, 1)
        status = await daemon.handle({"cmd": "status"})
        assert (status["connected"], status["on"]) == (True, True)
        assert (status["color"], status["bulb"]) == ("#ff0064", "#ff0064")
        await daemon.close()

    run(scenario())


def test_bad_requests_are_reported_not_raised():
    async def scenario():
        daemon = await attached(FakeBulbTransport())
        for request, fragment in (
            ({"cmd": "dance"}, "unknown command"),
            ({"cmd": "color"}, "missing field"),
            ({"cmd": "color", "color": "nope"}, "hex"),
            ({"cmd": "brightness", "level": 3}, "0..1"),
            ({"cmd": "effect", "name": "disco"}, "unknown effect"),
            ({"cmd": "effect", "name": "hue", "params": {"color": "red"}}, "no parameter"),
            ({"cmd": "native", "name": "fixed"}, "unknown native effect"),
        ):
            reply = await daemon.handle(request)
            assert not reply["ok"], request
            assert fragment in reply["error"], reply
        await daemon.close()

    run(scenario())


def test_rejected_effect_leaves_the_plan_and_the_capture_untouched():
    async def scenario():
        FakeMusic.created.clear()
        daemon = await attached(FakeBulbTransport())
        await daemon.handle({"cmd": "color", "color": "#0000ff"})
        reply = await daemon.handle({"cmd": "effect", "name": "music", "params": {"delay": 9}})
        assert "delay" in reply["error"]
        status = await daemon.handle({"cmd": "status"})
        assert (status["effect"], status["playing"], status["color"]) == (None, False, "#0000ff")
        assert not any(source.running for source in FakeMusic.created)
        await daemon.close()

    run(scenario())


def test_state_file_with_a_bad_effect_is_ignored(tmp_path):
    state_path = tmp_path / "state.json"
    state_path.write_text(
        json.dumps({"on": True, "color": "#102030", "brightness": 1.0, "effect": {"name": "disco", "params": {}}})
    )

    async def scenario():
        transport = FakeBulbTransport()
        transport.channels = [0, 0, 0, 255, 0]
        daemon = service.BulbService(ChSmartBulb(transport, auto_reconnect=False), state_path=state_path)
        await daemon.start()
        await until(lambda: bool(transport.opened))
        await asyncio.sleep(0.02)
        status = await daemon.handle({"cmd": "status"})
        assert (status["connected"], status["color"], status["effect"]) == (
            True,
            "#000000ff",
            None,
        )  # took over the bulb
        await daemon.close()

    run(scenario())


def test_effect_runs_in_the_background_and_stop_returns_to_the_plain_colour():
    async def scenario():
        transport = FakeBulbTransport()
        daemon = await attached(transport, fps=200)
        await daemon.handle({"cmd": "color", "color": "#0000ff"})
        sent = len(transport.light_bodies)
        reply = await daemon.handle({"cmd": "effect", "name": "strobe", "params": {"color": "red", "hz": 50}})
        assert reply["ok"]
        await until(lambda: len(transport.light_bodies) > sent + 3)
        assert (await daemon.handle({"cmd": "status"}))["playing"]
        await daemon.handle({"cmd": "brightness", "level": 0.5})  # dims without restarting the effect
        await until(lambda: transport.last_light["r"] == 128)
        await daemon.handle({"cmd": "stop"})
        assert not (await daemon.handle({"cmd": "status"}))["playing"]
        assert (transport.last_light["r"], transport.last_light["b"]) == (0, 128)
        await daemon.close()

    run(scenario())


def test_finite_effect_returns_to_the_plain_colour_when_done():
    async def scenario():
        transport = FakeBulbTransport()
        daemon = await attached(transport, fps=200)
        await daemon.handle({"cmd": "color", "color": "#0000ff"})
        await daemon.handle({"cmd": "effect", "name": "hue", "duration": 0.05})
        await daemon.wait_effect()
        status = await daemon.handle({"cmd": "status"})
        assert (status["effect"], status["playing"], status["bulb"]) == (None, False, "#0000ff")
        await daemon.close()

    run(scenario())


def test_sound_effect_starts_and_stops_the_audio_capture():
    async def scenario():
        FakeMusic.created.clear()
        daemon = await attached(FakeBulbTransport(), fps=200)
        await daemon.handle({"cmd": "effect", "name": "music"})
        source = FakeMusic.created[-1]
        assert source.running
        await daemon.handle({"cmd": "color", "color": "red"})
        assert not source.running
        await daemon.close()

    run(scenario())


def test_native_effect_and_bulb_queries():
    async def scenario():
        transport = FakeBulbTransport()
        daemon = await attached(transport)
        await daemon.handle({"cmd": "native", "name": "breathing", "color": "#00ff00", "speed": 4})
        assert (transport.last_light["effect"], transport.last_light["g"], transport.last_light["speed"]) == (
            0x52,
            255,
            0x41,
        )
        info = await daemon.handle({"cmd": "info"})
        assert (info["name"], info["model"]) == ("SmartBulb Bluetooth", "BL04")
        timers = (await daemon.handle({"cmd": "timers"}))["timers"]
        assert [(t["name"], t["enabled"]) for t in timers] == [("power off", True), ("power on", False)]
        changed = await daemon.handle({"cmd": "timer", "index": 6, "enabled": False})
        assert changed["timer"]["enabled"] is False
        raw = await daemon.handle({"cmd": "raw", "hex": "01fe0000510210000000008000000080"})
        assert raw["answer"] == "01fe000041021000470c000000000000"
        await daemon.close()

    run(scenario())


def test_state_is_remembered_and_restored_after_a_restart(tmp_path):
    state_path = tmp_path / "state" / "state.json"

    async def first():
        daemon = await attached(FakeBulbTransport(), state_path=state_path)
        await daemon.handle({"cmd": "color", "color": "#102030", "brightness": 0.5})
        await daemon.close()

    async def second():
        transport = FakeBulbTransport()  # a bulb that came back white after losing power
        transport.channels = [0, 0, 0, 255, 0]
        daemon = service.BulbService(ChSmartBulb(transport, auto_reconnect=False), state_path=state_path)
        await daemon.start()
        await until(lambda: transport.light_bodies and transport.last_light["w"] == 0)
        assert (transport.last_light["r"], transport.last_light["g"], transport.last_light["b"]) == (8, 16, 24)
        await daemon.close()

    run(first())
    assert json.loads(state_path.read_text())["color"] == "#102030"
    run(second())


def test_supervisor_reconnects_and_resumes_the_effect():
    async def scenario():
        transport = FakeBulbTransport()
        transport.fail_open = True
        bulb = ChSmartBulb(transport, auto_reconnect=False)
        daemon = service.BulbService(bulb, fps=200, retry_delay=0.01, poll_interval=0.01)
        await daemon.start()
        await asyncio.sleep(0.03)
        reply = await daemon.handle({"cmd": "effect", "name": "strobe", "params": {"hz": 50}})
        assert reply["ok"]  # accepted while the bulb is away
        assert not (await daemon.handle({"cmd": "status"}))["connected"]
        assert "not connected" in (await daemon.handle({"cmd": "info"}))["error"]

        transport.fail_open = False  # the bulb is switched on
        await until(lambda: len(transport.light_bodies) > 3)

        transport.drop_link()  # and loses power again
        sent = len(transport.light_bodies)
        await until(lambda: transport.opened >= 2 and len(transport.light_bodies) > sent + 3)
        assert (await daemon.handle({"cmd": "status"}))["playing"]
        await daemon.close()

    run(scenario())


@unix_sockets
def test_unusable_socket_path_is_a_clear_error(tmp_path):
    async def scenario():
        daemon = service.BulbService(ChSmartBulb(FakeBulbTransport(), auto_reconnect=False))
        with pytest.raises(SmartBulbError, match="cannot listen"):
            await daemon.serve(tmp_path / ("x" * 200) / "bulb.sock")

    run(scenario())


@unix_sockets
def test_requests_travel_over_the_socket_and_the_cli_uses_it(tmp_path, capsys):
    socket_path = tmp_path / "bulb.sock"

    async def scenario():
        transport = FakeBulbTransport()
        daemon = service.BulbService(ChSmartBulb(transport, auto_reconnect=False), retry_delay=0.01)
        server = asyncio.create_task(daemon.serve(socket_path))
        await until(lambda: socket_path.exists() and bool(transport.opened))
        assert await client.is_running(socket_path)

        reply = await client.call(socket_path, {"cmd": "color", "color": "#00ff00"})
        assert reply == {"ok": True}
        assert transport.last_light["g"] == 255

        # the command line talks to the same socket; it runs its own loop, so use a thread
        code = await asyncio.to_thread(main, ["--socket", str(socket_path), "rgb", "1", "2", "3"])
        assert code == 0
        assert (transport.last_light["r"], transport.last_light["g"], transport.last_light["b"]) == (1, 2, 3)
        assert await asyncio.to_thread(main, ["--socket", str(socket_path), "status"]) == 0

        server.cancel()
        await asyncio.gather(server, return_exceptions=True)
        assert not socket_path.exists()
        assert not await client.is_running(socket_path)

    run(scenario())
    out = capsys.readouterr().out
    assert "bulb:       connected" in out
    assert "colour:     #010203" in out
