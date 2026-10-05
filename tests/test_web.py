"""The web interface: static files and the WebSocket that carries the socket protocol."""

import asyncio
import base64
import gzip
import json
import os

import pytest

from chsmartbulb import ChSmartBulb, service, web
from chsmartbulb.cli import main
from chsmartbulb.errors import SmartBulbError
from fakes import FakeBulbTransport, FakeMusic, until

TOKEN = "s3cret"


def run(coroutine):
    return asyncio.run(coroutine)


@pytest.fixture
def root(tmp_path):
    (tmp_path / "assets").mkdir()
    (tmp_path / "index.html").write_text("<!doctype html><title>bulb</title>" + "<p>hello</p>" * 50)
    (tmp_path / "assets" / "app-abc123.js").write_text("console.log('bulb')")
    return tmp_path


class Site:
    """A service with the web interface on a loopback port, torn down on exit."""

    def __init__(self, root, **options):
        self.root = root
        self.transport = FakeBulbTransport()
        bulb = ChSmartBulb(self.transport, auto_reconnect=False)
        self.daemon = service.BulbService(bulb, music_factory=FakeMusic, retry_delay=0.01, **options)

    async def __aenter__(self):
        serving = self.daemon.serve(None, web=("127.0.0.1", 0), web_root=self.root, token=TOKEN)
        self.task = asyncio.create_task(serving)
        await until(lambda: self.daemon.web_address is not None and bool(self.transport.opened))
        assert self.daemon.web_address is not None
        self.port = self.daemon.web_address[1]
        return self

    async def __aexit__(self, *exc):
        self.task.cancel()
        await asyncio.gather(self.task, return_exceptions=True)

    async def http(self, request: str) -> tuple[int, dict[str, str], bytes]:
        reader, writer = await asyncio.open_connection("127.0.0.1", self.port)
        writer.write(request.replace("\n", "\r\n").encode() + b"\r\n\r\n")
        raw = await reader.read()
        writer.close()
        head, _, body = raw.partition(b"\r\n\r\n")
        status, *lines = head.decode().split("\r\n")
        headers = {name.lower(): value for name, _, value in (line.partition(": ") for line in lines)}
        return int(status.split()[1]), headers, body

    async def get(self, path: str, extra: str = "") -> tuple[int, dict[str, str], bytes]:
        return await self.http(f"GET {path} HTTP/1.1\nHost: bulb.local{extra}")

    async def socket(self, *, origin: str | None = None) -> "Socket":
        reader, writer = await asyncio.open_connection("127.0.0.1", self.port)
        key = base64.b64encode(os.urandom(16)).decode()
        lines = ["GET /ws HTTP/1.1", "Host: bulb.local", "Upgrade: websocket", "Connection: Upgrade"]
        lines += [f"Sec-WebSocket-Key: {key}", "Sec-WebSocket-Version: 13"]
        if origin is not None:
            lines.append(f"Origin: {origin}")
        writer.write("\r\n".join(lines).encode() + b"\r\n\r\n")
        head = await reader.readuntil(b"\r\n\r\n")
        return Socket(reader, writer, head.decode(), key)


class Socket:
    """Just enough of a WebSocket client to talk to the service."""

    def __init__(self, reader, writer, head, key):
        self.reader, self.writer, self.head, self.key = reader, writer, head, key
        self.status = int(head.split()[1])

    def send_frame(self, opcode: int, payload: bytes, *, fin: bool = True, masked: bool = True) -> None:
        mask = os.urandom(4)
        size = len(payload)
        if size < 126:
            length = bytes([size])
        elif size <= 0xFFFF:
            length = bytes([126]) + size.to_bytes(2, "big")
        else:
            length = bytes([127]) + size.to_bytes(8, "big")
        header = bytes([(0x80 if fin else 0) | opcode, length[0] | (0x80 if masked else 0)]) + length[1:]
        body = bytes(b ^ mask[i % 4] for i, b in enumerate(payload)) if masked else payload
        self.writer.write(header + (mask if masked else b"") + body)

    def send(self, message: dict) -> None:
        self.send_frame(0x1, json.dumps(message).encode())

    async def frame(self) -> tuple[int, bytes]:
        first, size = await asyncio.wait_for(self.reader.readexactly(2), 1.0)
        assert not size & 0x80  # a server never masks
        if size == 126:
            size = int.from_bytes(await self.reader.readexactly(2), "big")
        return first & 0x0F, await self.reader.readexactly(size)

    async def receive(self) -> dict:
        opcode, payload = await self.frame()
        assert opcode == 0x1
        return json.loads(payload)

    async def ask(self, **request) -> dict:
        self.send(request)
        return await self.receive()

    async def login(self) -> "Socket":
        assert await self.ask(cmd="auth", token=TOKEN) == {"ok": True}
        return self

    def close(self) -> None:
        self.writer.close()


def test_static_files_are_served_compressed_and_cacheable(root):
    async def scenario():
        async with Site(root) as site:
            status, headers, body = await site.get("/", "\nAccept-Encoding: gzip, br")
            assert (status, headers["content-type"]) == (200, "text/html; charset=utf-8")
            assert headers["content-encoding"] == "gzip"
            assert gzip.decompress(body) == (root / "index.html").read_bytes()
            assert headers["cache-control"] == "no-cache"

            status, headers, body = await site.get("/", f"\nIf-None-Match: {headers['etag']}")
            assert (status, body) == (304, b"")

            status, headers, body = await site.get("/assets/app-abc123.js")
            assert (status, body) == (200, b"console.log('bulb')")
            assert "content-encoding" not in headers  # the client did not ask for gzip
            assert "immutable" in headers["cache-control"]

            status, headers, body = await site.http("HEAD /index.html HTTP/1.1\nHost: bulb.local")
            assert (status, body) == (200, b"")
            assert int(headers["content-length"]) == (root / "index.html").stat().st_size

    run(scenario())


def test_anything_else_over_http_is_turned_away(root):
    async def scenario():
        async with Site(root) as site:
            assert (await site.get("/missing.js"))[0] == 404
            assert (await site.get("/../test_web.py"))[0] == 404
            assert (await site.get("/assets/../index.html"))[0] == 404
            assert (await site.http("POST / HTTP/1.1\nHost: bulb.local"))[0] == 405
            assert (await site.http("nonsense"))[0] == 400
            assert (await site.get("/", "\nX-Padding: " + "x" * 10000))[0] == 431
            assert (await site.get("/ws"))[0] == 400  # not an upgrade

    run(scenario())


def test_requests_and_replies_travel_over_the_websocket(root):
    async def scenario():
        async with Site(root) as site:
            ws = await site.socket(origin="http://bulb.local")
            assert ws.status == 101
            accept = base64.b64encode(web.accept_digest(ws.key)).decode()
            assert f"Sec-WebSocket-Accept: {accept}" in ws.head
            await ws.login()
            assert await ws.ask(cmd="color", color="#00ff00", id=7) == {"ok": True, "id": 7}
            assert site.transport.last_light["g"] == 255
            reply = await ws.ask(cmd="color", color="nope", id="x")
            assert (reply["ok"], reply["id"]) == (False, "x")

            # a message split into fragments, with a ping in between
            ws.send_frame(0x1, b'{"cmd": "col', fin=False)
            ws.send_frame(0x9, b"hi")
            ws.send_frame(0x0, b'or", "color": "#0000ff"}')
            assert await ws.frame() == (0xA, b"hi")
            assert await ws.receive() == {"ok": True}
            assert site.transport.last_light["b"] == 255

            ws.send_frame(0x8, (1000).to_bytes(2, "big"))
            assert (await ws.frame())[0] == 0x8
            ws.close()

    run(scenario())


def test_websocket_needs_the_token_and_the_same_origin(root):
    async def scenario():
        async with Site(root) as site:
            ws = await site.socket()
            reply = await ws.ask(cmd="color", color="#ff0000")
            assert reply == {"ok": False, "error": "not authorised"}
            assert (await ws.frame())[0] == 0x8  # and the service hangs up
            ws.close()

            ws = await site.socket()
            assert not (await ws.ask(cmd="auth", token="guess"))["ok"]
            ws.close()
            assert not site.transport.light_bodies

            assert (await site.socket(origin="http://evil.example")).status == 403

    run(scenario())


def test_broken_websocket_clients_are_dropped(root):
    async def scenario():
        async with Site(root) as site:
            ws = await (await site.socket()).login()
            ws.send_frame(0x1, b'{"cmd": "status"}', masked=False)
            assert (await ws.frame())[0] == 0x8
            ws.close()

            ws = await (await site.socket()).login()
            ws.send_frame(0x1, b"x" * 70000)
            opcode, payload = await ws.frame()
            assert (opcode, int.from_bytes(payload[:2], "big")) == (0x8, 1009)
            ws.close()

            ws = await (await site.socket()).login()  # the service still answers
            assert (await ws.ask(cmd="status"))["ok"]
            ws.close()

    run(scenario())


def test_subscribers_are_told_what_other_clients_change(root):
    async def scenario():
        async with Site(root, poll_interval=0.01) as site:
            watcher = await (await site.socket()).login()
            first = await watcher.ask(cmd="subscribe", id=1)
            assert (first["ok"], first["id"], first["connected"], first["playing"]) == (True, 1, True, False)

            other = await (await site.socket()).login()
            assert (await other.ask(cmd="color", color="#ff0000", brightness=0.5))["ok"]
            event = await watcher.receive()
            assert (event["event"], event["color"], event["brightness"]) == ("state", "#ff0000", 0.5)
            assert "ok" not in event

            site.transport.fail_open = True
            site.transport.drop_link()
            assert (await watcher.receive())["connected"] is False
            watcher.close()
            other.close()
            await until(lambda: not site.daemon._listeners)

    run(scenario())


def test_idle_websockets_are_pinged(root, monkeypatch):
    monkeypatch.setattr(web, "PING_INTERVAL", 0.01)

    async def scenario():
        async with Site(root) as site:
            ws = await (await site.socket()).login()
            assert (await ws.frame())[0] == 0x9
            ws.close()

    run(scenario())


def test_web_interface_needs_a_token_and_its_files(tmp_path):
    async def scenario():
        daemon = service.BulbService(ChSmartBulb(FakeBulbTransport(), auto_reconnect=False))
        with pytest.raises(SmartBulbError, match="needs a token"):
            await daemon.serve(None, web=("127.0.0.1", 0), web_root=tmp_path)
        with pytest.raises(SmartBulbError, match="not built"):
            await daemon.serve(None, web=("127.0.0.1", 0), web_root=tmp_path, token=TOKEN)

    run(scenario())


def test_cli_refuses_to_serve_the_web_interface_without_a_token(monkeypatch, capsys):
    monkeypatch.delenv("CHSMARTBULB_TOKEN", raising=False)
    assert main(["--address", "AA:BB:CC:DD:EE:FF", "daemon", "--web", "8378"]) == 1
    assert "--web needs a token" in capsys.readouterr().err


def test_web_interface_can_be_opened_to_everyone_on_purpose(root):
    async def scenario():
        transport = FakeBulbTransport()
        daemon = service.BulbService(ChSmartBulb(transport, auto_reconnect=False), retry_delay=0.01)
        serving = asyncio.create_task(daemon.serve(None, web=("127.0.0.1", 0), web_root=root, web_open=True))
        await until(lambda: daemon.web_address is not None and bool(transport.opened))
        site = Site(root)
        assert daemon.web_address is not None
        site.port = daemon.web_address[1]
        ws = await site.socket()
        assert (await ws.ask(cmd="subscribe"))["ok"]  # no token asked for
        assert (await ws.ask(cmd="color", color="#00ff00"))["ok"]
        ws.close()
        serving.cancel()
        await asyncio.gather(serving, return_exceptions=True)

    run(scenario())


def test_open_web_interface_does_not_open_the_tcp_port(root):
    async def scenario():
        daemon = service.BulbService(ChSmartBulb(FakeBulbTransport(), auto_reconnect=False))
        with pytest.raises(SmartBulbError, match="needs a token"):
            await daemon.serve(None, listen=("127.0.0.1", 0), web=("127.0.0.1", 0), web_root=root, web_open=True)

    run(scenario())
