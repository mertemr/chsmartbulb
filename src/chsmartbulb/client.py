"""Talking to a running service: one-off requests and the agents that feed it."""

from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import os
import socket
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any, Protocol

from .errors import ConnectionFailed, SmartBulbError
from .music import Capture, Levels, MusicSource
from .screen import PRIMARY, ScreenCapture, ScreenFeed, list_monitors

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping

    from .color import Color

    _Send = Callable[[Mapping[str, Any]], None]

log = logging.getLogger(__name__)

DEFAULT_PORT = 8377


@dataclass(frozen=True)
class Remote:
    """A service reached over the network."""

    host: str
    port: int = DEFAULT_PORT
    token: str = ""

    @classmethod
    def parse(cls, text: str, token: str = "") -> Remote:
        """Accept ``host`` or ``host:port``."""
        host, separator, port = text.rpartition(":")
        if not separator:
            return cls(text, DEFAULT_PORT, token)
        if not host or not port.isdigit():
            raise ValueError(f"expected HOST or HOST:PORT, got {text!r}")
        return cls(host, int(port), token)

    def __str__(self) -> str:
        return f"{self.host}:{self.port}"


def default_socket_path() -> Path:
    """Where the service on this machine listens."""
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    user = os.getuid() if hasattr(os, "getuid") else os.environ.get("USERNAME", "user")
    base = Path(runtime) if runtime else Path(tempfile.gettempdir()) / f"chsmartbulb-{user}"
    return base / "chsmartbulb.sock"


#: Where a service listens: a Unix socket path on this machine or a :class:`Remote`.
Target = Path | Remote


async def _exchange(
    reader: asyncio.StreamReader, writer: asyncio.StreamWriter, request: Mapping[str, Any], wait: float
) -> dict[str, Any]:
    writer.write(json.dumps(request).encode() + b"\n")
    await writer.drain()
    try:
        line = await asyncio.wait_for(reader.readline(), wait)
    except asyncio.TimeoutError:
        raise SmartBulbError("the service did not answer in time") from None
    if not line:
        raise SmartBulbError("the service closed the connection without answering")
    if line.startswith(b"HTTP/"):
        raise ConnectionFailed("that port is the web interface; use the one given to --listen (usually 8377)")
    try:
        return json.loads(line)
    except ValueError:
        raise ConnectionFailed("whatever answered is not a chsmartbulb service") from None


async def connect(target: Target, *, wait: float = 15.0) -> tuple[asyncio.StreamReader, asyncio.StreamWriter]:
    """Open a connection, presenting the token when the service is remote."""
    try:
        if isinstance(target, Remote):
            reader, writer = await asyncio.wait_for(asyncio.open_connection(target.host, target.port), wait)
        elif hasattr(asyncio, "open_unix_connection"):
            reader, writer = await asyncio.open_unix_connection(str(target))
        else:
            raise ConnectionFailed("local sockets are not available on this platform; use --host")
    except (OSError, asyncio.TimeoutError) as exc:
        raise ConnectionFailed(f"no service on {target}: {exc or 'timed out'}") from exc
    if isinstance(target, Remote):
        try:
            reply = await _exchange(reader, writer, {"cmd": "auth", "token": target.token}, wait)
        except (OSError, SmartBulbError):
            writer.close()
            raise
        if not reply.get("ok"):
            writer.close()
            if not target.token:
                raise ConnectionFailed(f"{target} wants a token: use --token or set $CHSMARTBULB_TOKEN")
            raise ConnectionFailed(f"{target} refused the token")
    return reader, writer


async def is_running(socket_path: Path) -> bool:
    """Whether a service answers on the local ``socket_path``."""
    if not hasattr(asyncio, "open_unix_connection"):
        return False
    try:
        _reader, writer = await asyncio.open_unix_connection(str(socket_path))
    except OSError:
        return False
    writer.close()
    return True


async def call(target: Target, request: Mapping[str, Any], *, wait: float = 15.0) -> dict[str, Any]:
    """Send one request to a running service and return its reply."""
    reader, writer = await connect(target, wait=wait)
    try:
        return await _exchange(reader, writer, request, wait)
    finally:
        writer.close()


async def run_agent(
    target: Target,
    *,
    source_factory: Callable[[], Capture] = MusicSource,
    retry_delay: float = 3.0,
    name: str | None = None,
) -> None:
    """Analyse the audio playing on this machine and stream the result to a service.

    Runs until cancelled. Only band levels and beats travel, never the audio itself.
    A lost connection or a failed capture is retried after ``retry_delay`` seconds.
    """

    def attach(source: Capture, send: _Send) -> None:
        source.on_block = _audio_forwarder(send)

    def hello() -> dict[str, Any]:
        return {"cmd": "hello", "kind": "audio", "name": name or socket.gethostname()}

    await _keep_streaming(target, source_factory, attach, "audio analysis", retry_delay, hello=hello)


async def run_screen_agent(
    target: Target,
    *,
    source_factory: Callable[..., ScreenFeed] = ScreenCapture,
    retry_delay: float = 3.0,
    name: str | None = None,
    monitors: Callable[[], list[dict[str, int]]] = list_monitors,
    monitor: int = PRIMARY,
) -> None:
    """Watch this machine's screen and stream its colour to a service.

    Runs until cancelled. Only one colour at a time travels, never the picture. The service
    may tell the agent to watch another monitor; ``monitor`` is the one it starts on.
    """
    watching = monitor

    def attach(source: ScreenFeed, send: _Send) -> None:
        def forward(color: Color) -> None:
            send({"cmd": "screen", "color": color.to_hex()})

        source.on_color = forward

    def hello() -> dict[str, Any]:
        return {
            "cmd": "hello",
            "kind": "screen",
            "name": name or socket.gethostname(),
            "monitors": monitors(),
            "monitor": watching,
        }

    def on_event(event: Mapping[str, Any]) -> ScreenFeed | None:
        nonlocal watching
        if event.get("event") != "monitor":
            return None
        try:
            chosen = int(event["monitor"])
        except (KeyError, TypeError, ValueError):
            return None
        if chosen == watching:
            return None
        watching = chosen
        log.info("the service asks for monitor %d", chosen)
        return source_factory(monitor=chosen)

    def make() -> ScreenFeed:
        # a reconnecting agent keeps watching what the service last asked for
        return source_factory() if watching == monitor else source_factory(monitor=watching)

    await _keep_streaming(target, make, attach, "the screen colour", retry_delay, hello=hello, on_event=on_event)


class _Source(Protocol):
    async def start(self) -> None: ...

    async def wait(self) -> None: ...

    async def stop(self) -> None: ...


async def _keep_streaming(
    target: Target,
    source_factory: Callable[[], Any],
    attach: Callable[[Any, _Send], None],
    what: str,
    retry_delay: float,
    *,
    hello: Callable[[], Mapping[str, Any]] | None = None,
    on_event: Callable[[Mapping[str, Any]], Any] | None = None,
) -> None:
    while True:
        try:
            await _stream(target, source_factory(), attach, what, hello, on_event)
        except SmartBulbError as exc:
            log.warning("%s; retrying in %g s", exc, retry_delay)
            log.debug("details", exc_info=exc)
        else:
            log.warning("the service closed the connection; retrying in %g s", retry_delay)
        await asyncio.sleep(retry_delay)


def _audio_forwarder(send: _Send) -> Callable[[Levels, float], None]:
    silent = False

    def forward(levels: Levels, onset: float) -> None:
        nonlocal silent
        sent = [round(v, 3) for v in (levels.bass, levels.mid, levels.treble)]
        quiet = not any(sent)
        if quiet and silent:
            return  # nothing to say while nothing plays
        silent = quiet
        message: dict[str, Any] = {"cmd": "audio", "levels": sent}
        if onset > 1.0:  # anything lower is never a beat
            message["onset"] = round(onset, 2)
        if balance := round(levels.balance, 2):
            message["balance"] = balance
        send(message)

    return forward


async def _listen(reader: asyncio.StreamReader, events: asyncio.Queue[Mapping[str, Any]]) -> None:
    """Pass on what the service says to this agent; ends when it hangs up."""
    while line := await reader.readline():
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if isinstance(event, dict):
            events.put_nowait(event)


async def _stream(
    target: Target,
    source: _Source,
    attach: Callable[[Any, _Send], None],
    what: str,
    hello: Callable[[], Mapping[str, Any]] | None = None,
    on_event: Callable[[Mapping[str, Any]], Any] | None = None,
) -> None:
    reader, writer = await connect(target)

    def send(message: Mapping[str, Any]) -> None:
        writer.write(json.dumps(message).encode() + b"\n")

    attach(source, send)
    if hello is not None:
        send(hello())
    await source.start()
    log.info("streaming %s to %s", what, target)
    events: asyncio.Queue[Mapping[str, Any]] = asyncio.Queue()
    closed = asyncio.ensure_future(_listen(reader, events))  # ends when the service hangs up
    capture = asyncio.ensure_future(source.wait())
    event = asyncio.ensure_future(events.get())
    try:
        while True:
            await asyncio.wait({closed, capture, event}, return_when=asyncio.FIRST_COMPLETED)
            if capture.done():
                capture.result()  # raises the capture error, if that is what ended it
                break
            if closed.done():
                break
            told, event = event.result(), asyncio.ensure_future(events.get())
            replacement = on_event(told) if on_event is not None else None
            if replacement is not None:  # asked to capture something else: swap without leaving the service
                capture.cancel()
                await source.stop()
                source = replacement
                attach(source, send)
                await source.start()
                capture = asyncio.ensure_future(source.wait())
    finally:
        closed.cancel()
        capture.cancel()
        event.cancel()
        await source.stop()
        writer.close()
        with contextlib.suppress(asyncio.CancelledError, OSError):
            await asyncio.gather(closed, capture, event, return_exceptions=True)
