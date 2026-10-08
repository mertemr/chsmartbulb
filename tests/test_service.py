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


def test_away_dims_or_turns_off_and_back_restores_the_plan():
    async def scenario():
        transport = FakeBulbTransport()
        daemon = await attached(transport, away_looks={"lock": "dim", "sleep": "off"})
        await daemon.handle({"cmd": "color", "color": "#ff0064", "brightness": 0.5})

        assert (await daemon.handle({"cmd": "away", "reason": "lock"}))["ok"]
        assert 0 < transport.last_light["r"] < 40  # dimmed, not off
        assert (await daemon.handle({"cmd": "status"}))["away"] == "lock"

        await daemon.handle({"cmd": "away", "reason": "sleep"})  # a later reason replaces the earlier one
        assert transport.channels == [0, 0, 0, 0, 0]

        assert (await daemon.handle({"cmd": "back"}))["ok"]
        assert (transport.last_light["r"], transport.last_light["b"]) == (128, 50)  # the plan, as it was
        status = await daemon.handle({"cmd": "status"})
        assert (status["away"], status["color"], status["on"]) == (None, "#ff0064", True)
        await daemon.close()

    run(scenario())


def test_away_stops_the_running_effect_and_back_resumes_it():
    async def scenario():
        daemon = await attached(FakeBulbTransport(), away_looks={"lock": "dim"}, fps=200)
        await daemon.handle({"cmd": "effect", "name": "hue"})
        assert daemon._playing
        await daemon.handle({"cmd": "away", "reason": "lock"})
        assert not daemon._playing
        await daemon._apply()  # a reconnect or an agent coming in must not end the away look
        assert not daemon._playing
        await daemon.handle({"cmd": "back"})
        assert daemon._playing
        await daemon.close()

    run(scenario())


def test_a_request_ends_away_and_odd_away_requests_do_nothing_harmful():
    async def scenario():
        transport = FakeBulbTransport()
        daemon = await attached(transport, away_looks={"lock": "dim"})
        await daemon.handle({"cmd": "color", "color": "#00ff00"})
        await daemon.handle({"cmd": "away", "reason": "lock"})
        await daemon.handle({"cmd": "color", "color": "#0000ff"})  # somebody uses the light
        assert (await daemon.handle({"cmd": "status"}))["away"] is None
        assert transport.last_light["b"] == 255

        written = len(transport.light_bodies)
        assert (await daemon.handle({"cmd": "back"}))["ok"]  # nothing to come back from
        assert len(transport.light_bodies) == written

        assert (await daemon.handle({"cmd": "away", "reason": "shutdown"}))["ok"]  # no look for it: ignored
        assert (await daemon.handle({"cmd": "status"}))["away"] is None
        bad = await daemon.handle({"cmd": "away", "reason": "boredom"})
        assert not bad["ok"]
        assert "reason" in bad["error"]
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
        # retuning the effect, or moving to another that listens, keeps the capture open
        await daemon.handle({"cmd": "effect", "name": "music", "params": {"delay": 0.2}})
        await daemon.handle({"cmd": "effect", "name": "spectrum"})
        assert [made for made in FakeMusic.created if made.running] == [source]
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


def test_effects_reply_describes_every_parameter_and_the_native_effects():
    async def scenario():
        daemon = await attached(FakeBulbTransport())
        reply = await daemon.handle({"cmd": "effects"})
        breathe = next(info for info in reply["effects"] if info["name"] == "breathe")
        assert breathe["schema"]["color"] == {"type": "color", "optional": False}
        assert breathe["schema"]["period"] == {"type": "number", "min": 0.2, "max": 60.0, "step": 0.1}
        assert "breathing" in reply["native"]["names"]
        assert "fixed" not in reply["native"]["names"]
        assert reply["native"]["speed"] == [0, 15]
        await daemon.close()

    run(scenario())


def test_subscribers_hear_each_change_once():
    async def scenario():
        daemon = await attached(FakeBulbTransport())
        heard = []
        unsubscribe = daemon.subscribe(heard.append)
        await daemon.handle({"cmd": "color", "color": "#00ff00"})
        assert [(state["color"], state["on"], state["connected"]) for state in heard] == [("#00ff00", True, True)]
        await daemon.handle({"cmd": "color", "color": "#00ff00"})  # nothing changed
        await daemon.handle({"cmd": "status"})
        assert len(heard) == 1
        await daemon.handle({"cmd": "effect", "name": "hue", "duration": 0.02})
        assert (heard[-1]["playing"], heard[-1]["effect"]["name"]) == (True, "hue")
        await until(lambda: heard[-1]["effect"] is None)  # the effect ran out by itself
        assert not heard[-1]["playing"]
        unsubscribe()
        await daemon.handle({"cmd": "off"})
        assert heard[-1]["on"]
        await daemon.close()

    run(scenario())


def test_state_is_written_once_after_a_burst_of_changes(tmp_path):
    state_path = tmp_path / "state.json"

    async def scenario():
        daemon = await attached(FakeBulbTransport(), state_path=state_path, save_delay=0.05)
        for color in ("#010000", "#020000", "#030000"):
            await daemon.handle({"cmd": "color", "color": color})
        assert not state_path.exists()  # dragging a slider must not hammer the disk
        await until(state_path.exists)
        assert json.loads(state_path.read_text())["color"] == "#030000"
        await daemon.close()

    run(scenario())


def test_state_says_why_the_bulb_is_away_and_a_request_can_retry_at_once():
    async def scenario():
        transport = FakeBulbTransport()
        transport.fail_open = True
        bulb = ChSmartBulb(transport, auto_reconnect=False)
        daemon = service.BulbService(bulb, retry_delay=30.0, monitor_lister=list)
        heard = []
        daemon.subscribe(heard.append)
        await daemon.start()
        await until(lambda: bool(heard) and heard[-1]["link"] == "waiting")
        assert heard[0]["link"] == "connecting"
        status = await daemon.handle({"cmd": "status"})
        assert (status["connected"], status["link"], status["problem"]) == (False, "waiting", "fake: host is down")

        transport.fail_open = False  # back in range, long before the next attempt is due
        assert (await daemon.handle({"cmd": "reconnect"}))["ok"]
        await until(lambda: heard[-1]["link"] == "connected")
        assert (heard[-1]["connected"], heard[-1]["problem"]) == (True, None)
        await daemon.close()

    run(scenario())


def test_state_counts_who_is_watching_and_feeding():
    async def scenario():
        daemon = await attached(FakeBulbTransport())
        first, second = [], []
        daemon.subscribe(first.append)
        assert not first  # a newcomer is told the state in its reply, not as a change
        leave = daemon.subscribe(second.append)
        assert (first[-1]["watchers"], second) == (2, [])
        await daemon._agent_joined("audio")
        assert first[-1]["agents"] == {"audio": 1, "screen": 0}
        assert first[-1]["audio"] == "agent"
        leave()
        assert first[-1]["watchers"] == 1
        await daemon.close()

    run(scenario())
