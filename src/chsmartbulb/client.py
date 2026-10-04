"""Talking to a running service: one-off requests and the audio agent."""

from __future__ import annotations

import asyncio
import contextlib
import json
import logging
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any

from .errors import ConnectionFailed, SmartBulbError
from .music import Capture, Levels, MusicSource

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping

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
    return json.loads(line)


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
) -> None:
    """Analyse the audio playing on this machine and stream the result to a service.

    Runs until cancelled. Only band levels and beats travel, never the audio itself.
    A lost connection or a failed capture is retried after ``retry_delay`` seconds.
    """
    while True:
        try:
            await _stream(target, source_factory())
        except SmartBulbError as exc:
            log.warning("%s; retrying in %g s", exc, retry_delay)
            log.debug("details", exc_info=exc)
        else:
            log.warning("the service closed the connection; retrying in %g s", retry_delay)
        await asyncio.sleep(retry_delay)


async def _stream(target: Target, source: Capture) -> None:
    reader, writer = await connect(target)
    silent = False

    def forward(levels: Levels, beat: bool) -> None:
        nonlocal silent
        sent = [round(v, 3) for v in (levels.bass, levels.mid, levels.treble)]
        quiet = not any(sent) and not beat
        if quiet and silent:
            return  # nothing to say while nothing plays
        silent = quiet
        message: dict[str, Any] = {"cmd": "audio", "levels": sent}
        if beat:
            message["beat"] = True
        writer.write(json.dumps(message).encode() + b"\n")

    source.on_block = forward
    await source.start()
    log.info("streaming audio analysis to %s", target)
    closed = asyncio.ensure_future(reader.read())  # the service sends nothing back; this ends when it hangs up
    capture = asyncio.ensure_future(source.wait())
    try:
        await asyncio.wait({closed, capture}, return_when=asyncio.FIRST_COMPLETED)
        if capture.done():
            capture.result()  # raises the capture error, if that is what ended it
    finally:
        closed.cancel()
        capture.cancel()
        await source.stop()
        writer.close()
        with contextlib.suppress(asyncio.CancelledError, OSError):
            await asyncio.gather(closed, capture, return_exceptions=True)
