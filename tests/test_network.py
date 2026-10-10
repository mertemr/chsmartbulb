"""Requests and agents over the network, against a stand-in for the service."""

import asyncio
import os

import pytest

from chsmartbulb import Color, client
from chsmartbulb.cli import main
from chsmartbulb.errors import ConnectionFailed
from chsmartbulb.music import Levels
from fakes import FakeHub, ScriptedScreen, ScriptedSource, until

MONITORS = [{"index": 1, "width": 1920, "height": 1080}, {"index": 2, "width": 2560, "height": 1440}]
STATUS = {
    "connected": True,
    "audio": "local",
    "screen": "local",
    "on": True,
    "color": "#00ff00",
    "brightness": 0.5,
    "effect": {"name": "breathe", "params": {}},
    "playing": True,
    "native": None,
    "bulb": "#00ff00",
}


def run(coroutine):
    return asyncio.run(coroutine)


async def stopped(*tasks):
    for task in tasks:
        task.cancel()
    await asyncio.gather(*tasks, return_exceptions=True)


def test_requests_work_over_tcp_with_the_token():
    async def scenario():
        async with FakeHub(replies={"status": STATUS}) as hub:
            assert await client.call(hub.remote, {"cmd": "color", "color": "#00ff00"}) == {"ok": True}
            assert hub.requests == [{"cmd": "color", "color": "#00ff00"}]
            assert (await client.call(hub.remote, {"cmd": "status"}))["audio"] == "local"

    run(scenario())


def test_wrong_or_missing_token_gets_nothing_done():
    async def scenario():
        async with FakeHub() as hub:
            wrong = client.Remote(hub.remote.host, hub.remote.port, "guess")
            with pytest.raises(ConnectionFailed, match="refused the token"):
                await client.call(wrong, {"cmd": "color", "color": "#ff0000"})
            missing = client.Remote(hub.remote.host, hub.remote.port)
            with pytest.raises(ConnectionFailed, match="wants a token"):
                await client.call(missing, {"cmd": "color", "color": "#ff0000"})
            assert not hub.requests

    run(scenario())


def test_audio_agent_says_who_it_is_and_sends_nothing_while_nothing_plays():
    async def scenario():
        async with FakeHub() as hub:
            faint = (Levels(mid=0.0004), 0.0)  # rounds to nothing on the wire
            blocks = [(Levels(), 0.0)] * 5 + [(Levels(bass=0.5, balance=-0.251), 2.5)] + [faint] * 5
            source = ScriptedSource(blocks, interval=0.001)
            agent = asyncio.create_task(client.run_agent(hub.remote, source_factory=lambda: source, name="desk"))
            await until(lambda: len(hub.sent("audio")) >= 3)
            await asyncio.sleep(0.03)
            await stopped(agent)
            assert hub.sent("hello") == [{"cmd": "hello", "kind": "audio", "name": "desk"}]
            # the first silent block, the sound, the first silent block after it
            assert hub.sent("audio") == [
                {"cmd": "audio", "levels": [0.0, 0.0, 0.0]},
                {"cmd": "audio", "levels": [0.5, 0.0, 0.0], "onset": 2.5, "balance": -0.25},
                {"cmd": "audio", "levels": [0.0, 0.0, 0.0]},
            ]

    run(scenario())


def test_screen_agent_offers_its_monitors_and_sends_the_colour():
    async def scenario():
        async with FakeHub() as hub:
            colors = [Color(r=10), Color(g=200, b=3)]
            agent = asyncio.create_task(
                client.run_screen_agent(
                    hub.remote, source_factory=lambda: ScriptedScreen(colors), name="desk", monitors=lambda: MONITORS
                )
            )
            await until(lambda: len(hub.sent("screen")) == 2)
            await stopped(agent)
            assert hub.sent("hello") == [
                {"cmd": "hello", "kind": "screen", "name": "desk", "monitors": MONITORS, "monitor": 1}
            ]
            assert [message["color"] for message in hub.sent("screen")] == ["#0a0000", "#00c803"]

    run(scenario())


def test_the_screen_agent_switches_monitor_when_told_to():
    async def scenario():
        async with FakeHub() as hub:
            made = []

            def factory(monitor=1):
                made.append(monitor)
                return ScriptedScreen([Color(r=10 * monitor)])

            agent = asyncio.create_task(
                client.run_screen_agent(hub.remote, source_factory=factory, monitors=lambda: MONITORS, retry_delay=0.01)
            )
            await until(lambda: len(hub.sent("screen")) == 1)
            hub.tell({"event": "state"})  # not for the agent
            hub.tell({"event": "monitor", "monitor": "soon"})  # nor anything it can follow
            hub.tell({"event": "monitor", "monitor": 2})
            await until(lambda: made == [1, 2] and len(hub.sent("screen")) == 2)
            assert hub.connections == 1  # the same connection, not a reconnect

            hub.hang_up()  # and after a reconnect it still watches what it was last asked for
            await until(lambda: hub.connections == 2 and len(hub.sent("hello")) == 2)
            assert (made, hub.sent("hello")[-1]["monitor"]) == ([1, 2, 2], 2)
            await stopped(agent)

    run(scenario())


def test_an_agent_comes_back_after_its_capture_fails():
    class Broken(ScriptedSource):
        async def wait(self):
            raise client.SmartBulbError("no sound device")

    async def scenario():
        async with FakeHub() as hub:
            sources = [Broken([]), ScriptedSource([(Levels(bass=0.5), 0.0)])]
            agent = asyncio.create_task(
                client.run_agent(hub.remote, source_factory=lambda: sources.pop(0), retry_delay=0.01)
            )
            await until(lambda: len(hub.sent("audio")) == 1)
            await stopped(agent)
            assert not sources
            assert hub.connections == 2

    run(scenario())


def test_remote_parses_host_and_port():
    assert client.Remote.parse("desk", "t") == client.Remote("desk", client.DEFAULT_PORT, "t")
    assert client.Remote.parse("10.0.0.5:9000", "t") == client.Remote("10.0.0.5", 9000, "t")
    assert str(client.Remote("desk", 9000)) == "desk:9000"
    with pytest.raises(ValueError):
        client.Remote.parse("desk:http")


def test_cli_reports_an_unreachable_host_and_a_missing_token(monkeypatch, capsys):
    monkeypatch.delenv("CHSMARTBULB_TOKEN", raising=False)
    assert main(["--host", "127.0.0.1:1", "status"]) == 1
    assert "no service on 127.0.0.1:1" in capsys.readouterr().err

    async def scenario():
        async with FakeHub() as hub:
            assert await asyncio.to_thread(main, ["--host", str(hub.remote), "status"]) == 1

    run(scenario())
    assert "wants a token: use --token" in capsys.readouterr().err


def test_cli_talks_to_a_remote_service(capsys):
    async def scenario():
        effects = {
            "effects": [{"name": "screensound", "summary": "both", "params": {}, "needs": "screen", "also": "audio"}]
        }
        async with FakeHub(replies={"status": STATUS, "effects": effects}) as hub:
            args = ["--host", str(hub.remote), "--token", "s3cret"]
            assert await asyncio.to_thread(main, [*args, "rgb", "9", "8", "7", "-b", "50"]) == 0
            assert await asyncio.to_thread(main, [*args, "effect", "hue", "-p", "3", "-s", "saturation=0.5"]) == 0
            assert await asyncio.to_thread(main, [*args, "status"]) == 0
            assert await asyncio.to_thread(main, [*args, "effects"]) == 0
            return hub.requests

    color, effect, *_ = run(scenario())
    assert color == {"cmd": "color", "color": "#090807", "fade": False, "brightness": 0.5}
    assert (effect["name"], effect["params"]) == ("hue", {"saturation": "0.5", "period": 3.0})
    out = capsys.readouterr().out
    assert "audio:      local" in out
    assert "effect:     breathe" in out
    assert "screensound both [screen, audio]" in out


def test_cli_sets_a_sleep_timer_and_shows_it(capsys):
    async def scenario():
        status = {**STATUS, "sleep": {"minutes": 30.0, "left": 1741.0}}
        async with FakeHub(replies={"status": status}) as hub:
            args = ["--host", str(hub.remote), "--token", "s3cret"]
            assert await asyncio.to_thread(main, [*args, "sleep", "30"]) == 0
            assert await asyncio.to_thread(main, [*args, "status"]) == 0
            return hub.requests

    assert run(scenario())[0] == {"cmd": "sleep", "minutes": 30.0}
    assert "sleep:      off in 30 min" in capsys.readouterr().out


def test_a_service_without_a_token_lets_everyone_in():
    async def scenario():
        async with FakeHub(token=None) as hub:
            assert await client.call(hub.remote, {"cmd": "on"}) == {"ok": True}
            assert await asyncio.to_thread(main, ["--host", str(hub.remote), "off"]) == 0  # no --token given
            assert [request["cmd"] for request in hub.requests] == ["on", "off"]

    run(scenario())


def test_platforms_without_unix_sockets_or_getuid_still_work(monkeypatch, tmp_path):
    monkeypatch.delattr(os, "getuid", raising=False)
    monkeypatch.delenv("XDG_RUNTIME_DIR", raising=False)
    monkeypatch.setenv("USERNAME", "mert")
    assert client.default_socket_path().parent.name == "chsmartbulb-mert"

    monkeypatch.delattr(asyncio, "open_unix_connection", raising=False)

    async def scenario():
        assert not await client.is_running(tmp_path / "bulb.sock")
        with pytest.raises(ConnectionFailed, match="--host"):
            await client.call(tmp_path / "bulb.sock", {"cmd": "status"})

    run(scenario())


@pytest.mark.skipif(not hasattr(asyncio, "start_unix_server"), reason="needs Unix sockets")
def test_cli_uses_the_service_on_the_local_socket(tmp_path, capsys):
    socket_path = tmp_path / "bulb.sock"
    asked = []

    async def session(reader, writer):
        while line := await reader.readline():
            asked.append(line)
            writer.write(b'{"ok": false, "error": "the bulb is not connected"}\n')
        writer.close()

    async def scenario():
        server = await asyncio.start_unix_server(session, str(socket_path))
        assert await client.is_running(socket_path)
        code = await asyncio.to_thread(main, ["--socket", str(socket_path), "on", "--fade"])
        server.close()
        return code

    assert run(scenario()) == 1  # no address was needed: the service holds the bulb
    assert asked == [b'{"cmd": "on", "fade": true}\n']
    assert "the bulb is not connected" in capsys.readouterr().err


def test_something_else_on_the_port_is_reported_not_raised():
    async def stranger(reader, writer):
        await reader.readline()
        writer.write(b"220 mail ready\r\n")
        writer.close()

    async def scenario():
        server = await asyncio.start_server(stranger, "127.0.0.1", 0)
        port = server.sockets[0].getsockname()[1]
        with pytest.raises(ConnectionFailed, match="not a chsmartbulb service"):
            await client.call(client.Remote("127.0.0.1", port, "s3cret"), {"cmd": "status"})
        server.close()

    run(scenario())
