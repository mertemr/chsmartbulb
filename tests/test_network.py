"""Reaching the service from other machines: TCP, the token, and the audio agent."""

import asyncio
import json
import os

import pytest

from chsmartbulb import ChSmartBulb, client, service
from chsmartbulb.cli import main
from chsmartbulb.errors import ConnectionFailed, SmartBulbError
from chsmartbulb.music import Levels, RemoteAudio
from fakes import FakeBulbTransport, FakeMusic, ScriptedSource

TOKEN = "s3cret"


def run(coroutine):
    return asyncio.run(coroutine)


async def until(condition, *, limit=1.0):
    deadline = asyncio.get_running_loop().time() + limit
    while not condition():
        assert asyncio.get_running_loop().time() < deadline, "condition was never met"
        await asyncio.sleep(0.005)


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
    remote = daemon._remote = RemoteAudio(clock=lambda: now[0])

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


def test_remote_parses_host_and_port():
    assert client.Remote.parse("desk", "t") == client.Remote("desk", client.DEFAULT_PORT, "t")
    assert client.Remote.parse("10.0.0.5:9000", "t") == client.Remote("10.0.0.5", 9000, "t")
    assert str(client.Remote("desk", 9000)) == "desk:9000"
    with pytest.raises(ValueError):
        client.Remote.parse("desk:http")


def test_cli_needs_a_token_for_host_and_reports_an_unreachable_one(monkeypatch, capsys):
    monkeypatch.delenv("CHSMARTBULB_TOKEN", raising=False)
    assert main(["--host", "127.0.0.1:1", "status"]) == 1
    assert "token" in capsys.readouterr().err
    assert main(["--host", "127.0.0.1:1", "--token", "x", "status"]) == 1
    assert "no service on 127.0.0.1:1" in capsys.readouterr().err


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
