"""Reaching the service from other machines: TCP, the token, and the audio agent."""

import asyncio
import json
import os

import pytest

from chsmartbulb import ChSmartBulb, Color, client, service
from chsmartbulb.cli import main
from chsmartbulb.errors import ConnectionFailed, SmartBulbError
from chsmartbulb.music import Levels, RemoteAudio
from fakes import FakeBulbTransport, FakeMusic, ScriptedScreen, ScriptedSource, until

TOKEN = "s3cret"


def run(coroutine):
    return asyncio.run(coroutine)


class Hub:
    """A service on a loopback TCP port, torn down on exit."""

    def __init__(self, **options):
        self.transport = FakeBulbTransport()
        bulb = ChSmartBulb(self.transport, auto_reconnect=False)
        self.daemon = service.BulbService(bulb, music_factory=FakeMusic, retry_delay=0.01, **options)

    async def __aenter__(self):
        self.task = asyncio.create_task(self.daemon.serve(None, listen=("127.0.0.1", 0), token=TOKEN))
        await until(lambda: self.daemon.tcp_address is not None and bool(self.transport.opened))
        assert self.daemon.tcp_address is not None
        self.remote = client.Remote("127.0.0.1", self.daemon.tcp_address[1], TOKEN)
        return self

    async def __aexit__(self, *exc):
        self.task.cancel()
        await asyncio.gather(self.task, return_exceptions=True)

    async def status(self):
        return await client.call(self.remote, {"cmd": "status"})


def test_requests_work_over_tcp_with_the_token():
    async def scenario():
        async with Hub() as hub:
            assert await client.call(hub.remote, {"cmd": "color", "color": "#00ff00"}) == {"ok": True}
            assert hub.transport.last_light["g"] == 255
            assert (await hub.status())["audio"] == "local"

    run(scenario())


def test_wrong_or_missing_token_gets_nothing_done():
    async def scenario():
        async with Hub() as hub:
            wrong = client.Remote(hub.remote.host, hub.remote.port, "guess")
            with pytest.raises(ConnectionFailed, match="refused the token"):
                await client.call(wrong, {"cmd": "color", "color": "#ff0000"})

            # skipping the auth step entirely
            reader, writer = await asyncio.open_connection(hub.remote.host, hub.remote.port)
            writer.write(b'{"cmd": "color", "color": "#ff0000"}\n')
            assert b"not authorised" in await reader.readline()
            assert await reader.readline() == b""  # and the service hangs up
            writer.close()
            assert not hub.transport.light_bodies

    run(scenario())


def test_listening_on_the_network_without_a_token_is_refused():
    async def scenario():
        daemon = service.BulbService(ChSmartBulb(FakeBulbTransport(), auto_reconnect=False))
        with pytest.raises(SmartBulbError, match="needs a token"):
            await daemon.serve(None, listen=("127.0.0.1", 0))

    run(scenario())


def test_agent_feed_drives_the_music_effect_and_local_capture_takes_over_when_it_leaves():
    async def scenario():
        FakeMusic.created.clear()
        async with Hub(fps=200) as hub:
            await client.call(hub.remote, {"cmd": "effect", "name": "music"})
            local = FakeMusic.created[-1]
            assert local.running  # no agent yet, so the hub listens itself
            await asyncio.sleep(0.03)
            assert not hub.transport.light_bodies or hub.transport.channels == [0, 0, 0, 0, 0]  # silence is dark

            blocks = [(Levels(bass=1.0), 8.0)] + [(Levels(bass=0.8), 0.9)] * 200
            agent = asyncio.create_task(
                client.run_agent(hub.remote, source_factory=lambda: ScriptedSource(blocks), retry_delay=0.01)
            )
            await until(lambda: max(hub.transport.channels) > 100)  # the beat from the agent lit the bulb
            assert (await hub.status())["audio"] == "agent"
            assert not local.running

            agent.cancel()
            await asyncio.gather(agent, return_exceptions=True)
            await until(lambda: FakeMusic.created[-1].running and FakeMusic.created[-1] is not local)
            status = await hub.status()
            assert (status["audio"], status["playing"]) == ("local", True)

    run(scenario())


def test_agent_sends_nothing_while_nothing_plays():
    async def scenario():
        async with Hub() as hub:
            sent = []
            faint = (Levels(mid=0.0004), 0.0)  # rounds to nothing on the wire
            blocks = [(Levels(), 0.0)] * 5 + [(Levels(bass=0.5, balance=-0.251), 2.5)] + [faint] * 5
            source = ScriptedSource(blocks, interval=0.001)

            real_write = asyncio.StreamWriter.write

            def spy(self, data):
                sent.append(data)
                real_write(self, data)

            asyncio.StreamWriter.write = spy
            try:
                agent = asyncio.create_task(client.run_agent(hub.remote, source_factory=lambda: source))
                await until(lambda: sum(b'"audio"' in d for d in sent) >= 3)
                await asyncio.sleep(0.03)
            finally:
                asyncio.StreamWriter.write = real_write
                agent.cancel()
                await asyncio.gather(agent, return_exceptions=True)
            audio = [d for d in sent if b'"audio"' in d]
            assert len(audio) == 3  # first silent block, the sound, the first silent block after it
            assert json.loads(audio[0]) == {"cmd": "audio", "levels": [0.0, 0.0, 0.0]}
            assert json.loads(audio[1]) == {"cmd": "audio", "levels": [0.5, 0.0, 0.0], "onset": 2.5, "balance": -0.25}

    run(scenario())


def test_audio_blocks_carry_onset_and_balance_and_older_agents_still_count():
    now = [0.0]
    daemon = service.BulbService(ChSmartBulb(FakeBulbTransport()), music_factory=FakeMusic)
    remote = daemon._feeds["audio"].remote = RemoteAudio(clock=lambda: now[0])

    def push(**block):
        now[0] += 1.0
        daemon._push_audio({"cmd": "audio", **block})
        return remote.levels, remote.beats

    assert push(levels=[1, 0.5, 0], onset=3, balance=-0.4) == (Levels(1.0, 0.5, 0.0, -0.4), 1)
    assert push(levels=[1, 0, 0], onset=1.2) == (Levels(1.0), 1)  # not enough of a rise
    remote.sensitivity = 1.0
    assert push(levels=[1, 0, 0], onset=1.2)[1] == 2
    assert push(levels=[1, 0, 0], beat=True)[1] == 3  # an agent from before the onset strength
    assert push(levels=[1, 0, 0], balance=9)[0].balance == 1.0
    assert push(levels="junk") == (Levels(1.0, balance=1.0), 3)
    assert push(levels=[0, 0, 0], onset="loud") == (Levels(1.0, balance=1.0), 3)


def test_screen_agent_colours_the_bulb_and_leaves_it_dark_when_it_goes():
    def no_capture():
        raise SmartBulbError("nothing to capture the screen with here")

    async def scenario():
        async with Hub(fps=200, screen_factory=no_capture) as hub:
            request = {"cmd": "effect", "name": "screen", "params": {"smoothing": 0, "saturation": 1, "white": 0}}
            reply = await client.call(hub.remote, request)
            assert (reply["ok"], reply["error"]) == (False, "nothing to capture the screen with here")

            shown = ScriptedScreen([Color(r=200, g=100)])
            agent = asyncio.create_task(client.run_screen_agent(hub.remote, source_factory=lambda: shown))
            feed = hub.daemon._feeds["screen"]
            await until(lambda: feed.agents == 1)
            assert (await hub.status())["screen"] == "agent"
            assert (await client.call(hub.remote, request))["ok"]
            await until(lambda: hub.transport.channels[:3] == [100, 0, 200])  # green, blue, red

            agent.cancel()
            await asyncio.gather(agent, return_exceptions=True)
            await until(lambda: feed.agents == 0 and not any(hub.transport.channels))
            status = await hub.status()
            assert (status["screen"], status["playing"]) == ("local", True)  # waiting for an agent to return

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
        async with Hub() as hub:
            assert await asyncio.to_thread(main, ["--host", str(hub.remote), "status"]) == 1

    run(scenario())
    assert "wants a token: use --token" in capsys.readouterr().err


def test_service_without_a_token_lets_remote_clients_and_agents_in(capsys):
    async def scenario():
        transport = FakeBulbTransport()
        daemon = service.BulbService(ChSmartBulb(transport, auto_reconnect=False), retry_delay=0.01)
        serving = asyncio.create_task(daemon.serve(None, listen=("127.0.0.1", 0), open_access=True))
        await until(lambda: daemon.tcp_address is not None and bool(transport.opened))
        assert daemon.tcp_address is not None
        remote = client.Remote("127.0.0.1", daemon.tcp_address[1])
        assert await client.call(remote, {"cmd": "color", "color": "#00ff00"}) == {"ok": True}
        assert await asyncio.to_thread(main, ["--host", str(remote), "status"]) == 0  # no --token given

        agent = asyncio.create_task(
            client.run_agent(remote, source_factory=lambda: ScriptedSource([(Levels(bass=0.5), 0.0)]), retry_delay=0.01)
        )
        await until(lambda: daemon._feeds["audio"].agents == 1)
        agent.cancel()
        serving.cancel()
        await asyncio.gather(agent, serving, return_exceptions=True)

    run(scenario())
    assert "audio:      local" in capsys.readouterr().out


def test_cli_talks_to_a_remote_service(capsys):
    async def scenario():
        async with Hub() as hub:
            args = ["--host", str(hub.remote), "--token", TOKEN]
            assert await asyncio.to_thread(main, [*args, "rgb", "9", "8", "7"]) == 0
            assert (hub.transport.last_light["r"], hub.transport.last_light["g"]) == (9, 8)
            assert await asyncio.to_thread(main, [*args, "status"]) == 0

    run(scenario())
    assert "audio:      local" in capsys.readouterr().out


def test_platforms_without_unix_sockets_or_getuid_still_work(monkeypatch, tmp_path):
    monkeypatch.delattr(os, "getuid", raising=False)
    monkeypatch.delenv("XDG_RUNTIME_DIR", raising=False)
    monkeypatch.setenv("USERNAME", "mert")
    assert service.default_socket_path().parent.name == "chsmartbulb-mert"

    monkeypatch.delattr(asyncio, "open_unix_connection", raising=False)

    async def scenario():
        assert not await client.is_running(tmp_path / "bulb.sock")
        with pytest.raises(ConnectionFailed, match="--host"):
            await client.call(tmp_path / "bulb.sock", {"cmd": "status"})

    run(scenario())


def test_something_else_on_the_port_is_reported_not_raised():
    async def stranger(reader, writer):
        await reader.readline()
        writer.write(b"220 mail ready\r\n")
        writer.close()

    async def scenario():
        server = await asyncio.start_server(stranger, "127.0.0.1", 0)
        port = server.sockets[0].getsockname()[1]
        with pytest.raises(ConnectionFailed, match="not a chsmartbulb service"):
            await client.call(client.Remote("127.0.0.1", port, TOKEN), {"cmd": "status"})
        server.close()

    run(scenario())
