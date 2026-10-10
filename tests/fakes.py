"""In-memory stand-ins for hardware, modelled on behaviour observed on the real bulb."""

from __future__ import annotations

import asyncio
import contextlib
import json
from typing import TYPE_CHECKING, Any

import pytest

from chsmartbulb import _native, client
from chsmartbulb import protocol as p
from chsmartbulb.color import Color
from chsmartbulb.errors import ConnectionFailed, TransportError
from chsmartbulb.light import Light
from chsmartbulb.transport import Transport

if TYPE_CHECKING:
    from collections.abc import Callable

    from typing_extensions import Self

    from chsmartbulb.music import Levels

NAME_ANSWER = bytes.fromhex(
    "01fe0000418050000000000000000000312e302e00000003536d61727442756c6220426c7565746f6f7468"
) + bytes(37)
HARDWARE_ANSWER = bytes.fromhex("01fe0000418116007c58000034304c42010100001a00")
TIMERS_ANSWER = bytes.fromhex(
    "01fe0000413068000200000000000000"
    "706f776572206f666600010000000000120000001300000014000000150000000601017f0614000301000000"
    "706f776572206f6e0000010000000000120000001300000014000000150000000501007f0a23000301000000"
)
STATUS_ANSWER = bytes.fromhex("01fe0000410028000000000000000000001f00160000000500000200000000000004881102100000")


class FakeBulbTransport(Transport):
    """Answers queries like the real bulb and records every light command."""

    def __init__(self, *, chunk: int | None = None) -> None:
        self.chunk = chunk  # split answers into pieces of this size, like BLE notifications
        self.opened = 0
        self.fail_open = False
        self.fail_next_write = False
        self.light_bodies: list[bytes] = []
        self.channels = [0, 0, 0, 0, 0]  # g, b, r, w, y as last written
        self.timers_answer = bytearray(TIMERS_ANSWER)
        self.timer_writes: list[bytes] = []
        self.ignore_timer_writes = False
        self._open = False
        self._incoming: asyncio.Queue[bytes | None] = asyncio.Queue()
        self._reader = p.FrameReader()

    @property
    def is_open(self) -> bool:
        return self._open

    async def open(self) -> None:
        if self.fail_open:
            raise ConnectionFailed("fake: host is down")
        self._open = True
        self.opened += 1
        self._incoming = asyncio.Queue()
        self._reader = p.FrameReader()

    async def close(self) -> None:
        if self._open:
            self._open = False
            self._incoming.put_nowait(None)

    def drop_link(self) -> None:
        """Simulate the bulb going out of range."""
        self._open = False
        self._incoming.put_nowait(None)

    async def write(self, data: bytes) -> None:
        if not self._open:
            raise TransportError("fake: closed")
        if self.fail_next_write:
            self.fail_next_write = False
            self._open = False
            self._incoming.put_nowait(None)
            raise TransportError("fake: broken pipe")
        for frame in self._reader.feed(data):
            self._handle(frame)

    async def read(self) -> bytes:
        data = await self._incoming.get()
        if data is None:
            raise TransportError("fake: closed")
        return data

    def _answer(self, data: bytes) -> None:
        size = self.chunk or len(data)
        for i in range(0, len(data), size):
            self._incoming.put_nowait(data[i : i + size])

    def _handle(self, frame: p.Frame) -> None:
        if frame.type == p.FrameType.SET and frame.command == p.Command.LIGHT:
            g, b, r, _speed, effect, w, y, _fade = frame.body
            try:
                p.NativeEffect(effect)
            except ValueError:
                return  # the real bulb ignores frames with an unknown effect byte
            self.light_bodies.append(frame.body)
            self.channels = [g, b, r, w, y]
        elif frame.type == p.FrameType.SET and frame.command == p.Command.TIMERS:
            self.timer_writes.append(frame.encode())
            if not self.ignore_timer_writes:
                # stored record = name[32] + tail, where the bulb reports tail[1] as 01
                record = bytearray(frame.body[8:])
                record[33] = 1
                index = record[32]
                offset = next(o for o in range(16, len(self.timers_answer), 44) if self.timers_answer[o + 32] == index)
                self.timers_answer[offset + 32 : offset + 44] = record[32:]
        elif frame.type == p.FrameType.QUERY:
            if frame.command == p.Command.IDENTIFY:
                self._answer(p.Frame(p.FrameType.ANSWER, frame.command, bytes.fromhex("470c000000000000")).encode())
            elif frame.command == p.Command.LIGHT_STATE:
                # the real bulb rescales so that the largest channel reads 255
                top = max(self.channels)
                g, b, r, w, y = (c * 255 // top if top else 0 for c in self.channels)
                self._answer(p.Frame(p.FrameType.ANSWER, frame.command, bytes((g, b, r, 0, 0, w, y, 0))).encode())
            elif frame.command == p.Command.NAME:
                self._answer(NAME_ANSWER)
            elif frame.command == p.Command.HARDWARE:
                self._answer(HARDWARE_ANSWER)
            elif frame.command == p.Command.TIMERS:
                self._answer(bytes(self.timers_answer))
            elif frame.command == p.Command.STATUS:
                self._answer(STATUS_ANSWER)

    @property
    def last_light(self) -> dict[str, int]:
        g, b, r, speed, effect, w, y, fade = self.light_bodies[-1]
        return dict(r=r, g=g, b=b, w=w, y=y, speed=speed, effect=effect, fade=fade)


class RecordingLight(Light):
    """A light that only remembers what it was told, with timestamps."""

    def __init__(self) -> None:
        self.colors: list[Color] = []
        self._brightness = 1.0
        self._connected = False

    async def connect(self) -> None:
        self._connected = True

    async def disconnect(self) -> None:
        self._connected = False

    @property
    def is_connected(self) -> bool:
        return self._connected

    @property
    def brightness(self) -> float:
        return self._brightness

    async def set_color(self, color: Color, *, fade: bool = False) -> None:
        self.colors.append(color)

    async def set_brightness(self, level: float) -> None:
        self._brightness = level

    async def turn_on(self, *, fade: bool = False) -> None:
        pass

    async def turn_off(self, *, fade: bool = False) -> None:
        self.colors.append(Color())


class ScriptedSource:
    """Stands in for a capturing :class:`MusicSource`: emits the given blocks, then idles.

    Each block is the levels and the onset strength that came with them.
    """

    def __init__(self, blocks: list[tuple[Levels, float]], interval: float = 0.005) -> None:
        self.blocks = blocks
        self.interval = interval
        self.on_block: Callable[[Levels, float], None] | None = None
        self._task: asyncio.Task[None] | None = None

    async def start(self) -> None:
        self._task = asyncio.create_task(self._emit())

    async def _emit(self) -> None:
        for levels, onset in self.blocks:
            if self.on_block is not None:
                self.on_block(levels, onset)
            await asyncio.sleep(self.interval)
        await asyncio.Event().wait()  # a real capture never ends on its own

    async def wait(self) -> None:
        if self._task is not None:
            await self._task

    async def stop(self) -> None:
        task, self._task = self._task, None
        if task is not None:
            task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await task


class ScriptedScreen:
    """Stands in for a :class:`chsmartbulb.screen.ScreenCapture`: reports the given colours, then idles."""

    def __init__(self, colors: list[Color], interval: float = 0.005) -> None:
        self.colors = colors
        self.interval = interval
        self.color = Color()
        self.on_color: Callable[[Color], None] | None = None
        self._task: asyncio.Task[None] | None = None

    async def start(self) -> None:
        self._task = asyncio.create_task(self._emit())

    async def _emit(self) -> None:
        for color in self.colors:
            self.color = color
            if self.on_color is not None:
                self.on_color(color)
            await asyncio.sleep(self.interval)
        await asyncio.Event().wait()

    async def wait(self) -> None:
        if self._task is not None:
            await self._task

    async def stop(self) -> None:
        task, self._task = self._task, None
        if task is not None:
            task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await task


class FakeHub:
    """Stands in for a service on a loopback port.

    Asks for the token as the real one does, answers every request with ``ok`` and what
    ``replies`` holds for its command, and keeps what it was asked (``requests``) and what
    agents streamed to it (``streamed``).
    """

    def __init__(self, token: str | None = "s3cret", replies: dict[str, dict[str, Any]] | None = None) -> None:
        self.token = token
        self.replies = replies or {}
        self.requests: list[dict[str, Any]] = []
        self.streamed: list[dict[str, Any]] = []
        self.agents: list[asyncio.StreamWriter] = []
        self.connections = 0

    async def __aenter__(self) -> Self:
        self.server = await asyncio.start_server(self._session, "127.0.0.1", 0)
        port = self.server.sockets[0].getsockname()[1]
        self.remote = client.Remote("127.0.0.1", port, self.token or "")
        return self

    async def __aexit__(self, *exc: object) -> None:
        self.hang_up()
        self.server.close()

    def tell(self, event: dict[str, Any]) -> None:
        """Send ``event`` to every agent, as the service does when it wants something of them."""
        for agent in self.agents:
            agent.write(json.dumps(event).encode() + b"\n")

    def hang_up(self) -> None:
        for agent in self.agents:
            agent.close()
        self.agents.clear()

    def sent(self, command: str) -> list[dict[str, Any]]:
        return [message for message in self.streamed if message["cmd"] == command]

    async def _session(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        self.connections += 1
        trusted = self.token is None
        with contextlib.suppress(ConnectionError):
            while line := await reader.readline():
                message = json.loads(line)
                command = message.get("cmd")
                if command == "auth":
                    trusted = trusted or message.get("token") == self.token
                    reply: dict[str, Any] = {"ok": trusted} if trusted else {"ok": False, "error": "wrong token"}
                elif not trusted:
                    writer.write(b'{"ok": false, "error": "not authorised"}\n')
                    break
                elif command in ("hello", "audio", "screen"):
                    if writer not in self.agents:
                        self.agents.append(writer)
                    self.streamed.append(message)
                    continue  # an agent's stream is not answered
                else:
                    self.requests.append(message)
                    reply = {"ok": True, **self.replies.get(command, {})}
                writer.write(json.dumps(reply).encode() + b"\n")
        writer.close()


def needs_native() -> None:
    """Skip the test where the Rust core is not installed, or is switched off."""
    if _native.load() is None:
        pytest.skip("needs chsmartbulb-native")


async def until(condition: Callable[[], bool], *, limit: float = 1.0) -> None:
    """Wait for ``condition`` to hold, failing the test when it never does."""
    deadline = asyncio.get_running_loop().time() + limit
    while not condition():
        assert asyncio.get_running_loop().time() < deadline, "condition was never met"
        await asyncio.sleep(0.005)
