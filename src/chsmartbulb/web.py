"""The web interface: static files and a WebSocket, with nothing but the standard library.

The page is a prebuilt bundle (see ``web/`` in the repository) and holds no logic
of the service. It talks over ``/ws``, where every text message is one object of
the socket protocol, so the same page works against anything that serves these
files and answers that protocol, a microcontroller included.
"""

from __future__ import annotations

import asyncio
import base64
import contextlib
import gzip
import hashlib
import json
import logging
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any
from urllib.parse import urlsplit

from .errors import SmartBulbError

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable, Mapping

    Receive = Callable[[], Awaitable["str | bytes | None"]]
    Send = Callable[[Mapping[str, Any]], Awaitable[None]]
    Session = Callable[[Receive, Send], Awaitable[None]]

log = logging.getLogger(__name__)

DEFAULT_PORT = 8378
PING_INTERVAL = 20.0  # seconds between pings on a WebSocket
IDLE_TIMEOUT = 60.0  # a client silent for this long, pongs included, is gone
MAX_HEADER = 8192
MAX_MESSAGE = 65536
_HEADER_TIMEOUT = 10.0
_WEBSOCKET_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

# fmt: off
_TEXT, _CLOSE, _PING, _PONG = 0x1, 0x8, 0x9, 0xA
_PROTOCOL_ERROR, _TOO_BIG   = 1002, 1009
# fmt: on

_TYPES = {
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".css": "text/css; charset=utf-8",
    ".json": "application/json",
    ".webmanifest": "application/manifest+json",
    ".svg": "image/svg+xml",
    ".png": "image/png",
    ".ico": "image/x-icon",
    ".woff2": "font/woff2",
    ".txt": "text/plain; charset=utf-8",
}
_REASONS = {
    101: "Switching Protocols",
    200: "OK",
    304: "Not Modified",
    400: "Bad Request",
    403: "Forbidden",
    404: "Not Found",
    405: "Method Not Allowed",
    431: "Request Header Fields Too Large",
}
_SECURITY = {
    "X-Content-Type-Options": "nosniff",
    "Referrer-Policy": "no-referrer",
    "Content-Security-Policy": (
        "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; "
        "connect-src 'self' ws: wss:; frame-ancestors 'none'"
    ),
}


def default_root() -> Path:
    """Where the built page is installed."""
    return Path(__file__).parent / "webui"


def accept_digest(key: str) -> bytes:
    """The proof a WebSocket server gives that it read the client's handshake key."""
    return hashlib.sha1((key + _WEBSOCKET_GUID).encode()).digest()


@dataclass(frozen=True)
class Asset:
    content_type: str
    body: bytes
    gzipped: bytes | None  # kept only where it is smaller
    etag: str
    immutable: bool  # the name carries a hash of the content


def load_assets(root: Path) -> dict[str, Asset]:
    """Read every file under ``root`` into memory, keyed by its path in a URL."""
    assets = {}
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        name = path.relative_to(root).as_posix()
        body = path.read_bytes()
        packed = gzip.compress(body, mtime=0)
        assets[name] = Asset(
            content_type=_TYPES.get(path.suffix, "application/octet-stream"),
            body=body,
            gzipped=packed if len(packed) < len(body) else None,
            etag=f'"{hashlib.sha1(body).hexdigest()[:16]}"',
            immutable=name.startswith("assets/"),
        )
    if "index.html" not in assets:
        raise SmartBulbError(f"the web interface is not built: no index.html in {root}")
    return assets


class _Closed(Exception):
    """The WebSocket has to end; ``code`` says why."""

    def __init__(self, code: int) -> None:
        super().__init__(code)
        self.code = code


class _WebSocket:
    """The server side of one WebSocket, after the handshake."""

    def __init__(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        self._reader = reader
        self._writer = writer

    def frame(self, opcode: int, payload: bytes = b"") -> None:
        size = len(payload)
        if size < 126:
            header = bytes((0x80 | opcode, size))
        elif size < 0x10000:
            header = bytes((0x80 | opcode, 126)) + size.to_bytes(2, "big")
        else:
            header = bytes((0x80 | opcode, 127)) + size.to_bytes(8, "big")
        self._writer.write(header + payload)

    async def send(self, message: Mapping[str, Any]) -> None:
        self.frame(_TEXT, json.dumps(message).encode())
        await self._writer.drain()

    async def receive(self) -> str | None:
        """The next text message, or ``None`` once the client has closed."""
        message = b""
        while True:
            first, second = await asyncio.wait_for(self._reader.readexactly(2), IDLE_TIMEOUT)
            if not second & 0x80:
                raise _Closed(_PROTOCOL_ERROR)  # a client must mask what it sends
            size = second & 0x7F
            if size == 126:
                size = int.from_bytes(await self._reader.readexactly(2), "big")
            elif size == 127:
                size = int.from_bytes(await self._reader.readexactly(8), "big")
            if len(message) + size > MAX_MESSAGE:
                raise _Closed(_TOO_BIG)
            mask = await self._reader.readexactly(4)
            payload = bytes(byte ^ mask[i % 4] for i, byte in enumerate(await self._reader.readexactly(size)))
            opcode = first & 0x0F
            if opcode == _CLOSE:
                return None
            if opcode == _PING:
                self.frame(_PONG, payload)
            elif opcode != _PONG:
                message += payload
                if first & 0x80:  # the last fragment
                    return message.decode("utf-8", "replace")

    async def keep_alive(self) -> None:
        """Ping now and then: the pongs keep :meth:`receive` from timing out on a quiet client."""
        with contextlib.suppress(ConnectionError):
            while True:
                await asyncio.sleep(PING_INTERVAL)
                self.frame(_PING)
                await self._writer.drain()


class Site:
    """Serves the page's files and hands each WebSocket to ``session``."""

    def __init__(self, session: Session, root: Path | None = None) -> None:
        self._session = session
        self._assets = load_assets(root or default_root())

    async def listen(self, host: str, port: int) -> asyncio.Server:
        return await asyncio.start_server(self._serve, host, port, limit=MAX_HEADER)

    async def _serve(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        try:
            head = await asyncio.wait_for(self._head(reader), _HEADER_TIMEOUT)
        except asyncio.LimitOverrunError:
            _respond(writer, 431)
        except (asyncio.IncompleteReadError, asyncio.TimeoutError, ConnectionError) as exc:
            if getattr(exc, "partial", b""):
                _respond(writer, 400)
        else:
            with contextlib.suppress(ConnectionError):
                await self._route(head, reader, writer)
        with contextlib.suppress(ConnectionError):
            await writer.drain()
        writer.close()

    @staticmethod
    async def _head(reader: asyncio.StreamReader) -> bytes:
        first = await reader.read(1)
        if first == b"{":
            # a client of the socket protocol on the wrong port: say so now, it will not send more
            raise asyncio.IncompleteReadError(first, None)
        return first + await reader.readuntil(b"\r\n\r\n")

    async def _route(self, head: bytes, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        request, *lines = head.decode("latin-1").split("\r\n")
        parts = request.split(" ")
        if len(parts) != 3 or not parts[1].startswith("/"):
            _respond(writer, 400)
            return
        method, path = parts[0], parts[1].partition("?")[0]
        headers = {name.strip().lower(): value.strip() for name, _, value in (line.partition(":") for line in lines)}
        if path == "/ws":
            await self._websocket(headers, reader, writer)
        elif method not in ("GET", "HEAD"):
            _respond(writer, 405, {"Allow": "GET, HEAD"})
        else:
            self._file(method, path, headers, writer)

    def _file(self, method: str, path: str, headers: Mapping[str, str], writer: asyncio.StreamWriter) -> None:
        asset = self._assets.get(path[1:] or "index.html")  # only names read from disk match, so no way out of root
        if asset is None:
            _respond(writer, 404)
            return
        caching = "public, max-age=31536000, immutable" if asset.immutable else "no-cache"
        fields = {"ETag": asset.etag, "Cache-Control": caching, "Vary": "Accept-Encoding", **_SECURITY}
        if headers.get("if-none-match") == asset.etag:
            _respond(writer, 304, fields)
            return
        body = asset.body
        accepted = [coding.partition(";")[0].strip() for coding in headers.get("accept-encoding", "").split(",")]
        if asset.gzipped is not None and "gzip" in accepted:
            body = asset.gzipped
            fields["Content-Encoding"] = "gzip"
        fields["Content-Type"] = asset.content_type
        _respond(writer, 200, fields, body, send_body=method == "GET")

    async def _websocket(
        self, headers: Mapping[str, str], reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> None:
        key = headers.get("sec-websocket-key")
        if headers.get("upgrade", "").lower() != "websocket" or not key or headers.get("sec-websocket-version") != "13":
            _respond(writer, 400)
            return
        origin = headers.get("origin")
        if origin is not None and urlsplit(origin).netloc != headers.get("host"):
            # a page from elsewhere, open in a browser on this network, must not reach the bulb
            _respond(writer, 403)
            return
        accept = base64.b64encode(accept_digest(key)).decode()
        _respond(writer, 101, {"Upgrade": "websocket", "Connection": "Upgrade", "Sec-WebSocket-Accept": accept})
        socket = _WebSocket(reader, writer)
        pinging = asyncio.create_task(socket.keep_alive())
        code = 1000
        try:
            await self._session(socket.receive, socket.send)
        except _Closed as exc:
            code = exc.code
        except (asyncio.IncompleteReadError, asyncio.TimeoutError):
            return  # gone without a word
        finally:
            pinging.cancel()
        socket.frame(_CLOSE, code.to_bytes(2, "big"))


def _respond(
    writer: asyncio.StreamWriter,
    status: int,
    fields: Mapping[str, str] | None = None,
    body: bytes = b"",
    *,
    send_body: bool = True,
) -> None:
    lines = [f"HTTP/1.1 {status} {_REASONS[status]}"]
    if status != 101:
        lines += [f"Content-Length: {len(body)}", "Connection: close"]
    lines += [f"{name}: {value}" for name, value in (fields or {}).items()]
    writer.write("\r\n".join(lines).encode("latin-1") + b"\r\n\r\n" + (body if send_body else b""))
