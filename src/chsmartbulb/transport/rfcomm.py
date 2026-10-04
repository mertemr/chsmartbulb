"""Bluetooth Classic serial port (SPP over RFCOMM) using only the standard library."""

from __future__ import annotations

import asyncio
import errno
import socket

from ..errors import ConnectionFailed, TransportError
from .base import Transport


class RfcommTransport(Transport):
    """RFCOMM stream socket to ``address`` on ``channel``.

    The device must already be paired. Reopening a channel right after closing
    it fails with ``EBUSY`` for a moment, so :meth:`open` retries that case.
    """

    def __init__(
        self,
        address: str,
        channel: int,
        *,
        connect_timeout: float = 10.0,
        busy_retries: int = 6,
    ) -> None:
        self.address = address
        self.channel = channel
        self._connect_timeout = connect_timeout
        self._busy_retries = busy_retries
        self._sock: socket.socket | None = None

    @property
    def is_open(self) -> bool:
        return self._sock is not None

    async def open(self) -> None:
        if self._sock is not None:
            return
        if not hasattr(socket, "AF_BLUETOOTH"):
            raise ConnectionFailed("this Python build has no Bluetooth socket support")
        loop = asyncio.get_running_loop()
        target = f"{self.address} channel {self.channel}"
        for attempt in range(self._busy_retries + 1):
            sock = socket.socket(socket.AF_BLUETOOTH, socket.SOCK_STREAM, socket.BTPROTO_RFCOMM)
            sock.setblocking(False)
            try:
                await asyncio.wait_for(
                    loop.sock_connect(sock, (self.address, self.channel)), self._connect_timeout
                )
            except asyncio.TimeoutError:
                sock.close()
                raise ConnectionFailed(f"timed out connecting to {target}") from None
            except OSError as exc:
                sock.close()
                if exc.errno == errno.EBUSY and attempt < self._busy_retries:
                    await asyncio.sleep(0.4 * (attempt + 1))
                    continue
                raise ConnectionFailed(f"cannot connect to {target}: {exc}") from exc
            self._sock = sock
            return

    async def close(self) -> None:
        sock, self._sock = self._sock, None
        if sock is not None:
            sock.close()

    async def write(self, data: bytes) -> None:
        if self._sock is None:
            raise TransportError("RFCOMM channel is closed")
        try:
            await asyncio.get_running_loop().sock_sendall(self._sock, data)
        except OSError as exc:
            raise TransportError(f"RFCOMM write failed: {exc}") from exc

    async def read(self) -> bytes:
        sock = self._sock
        if sock is None:
            raise TransportError("RFCOMM channel is closed")
        try:
            data = await asyncio.get_running_loop().sock_recv(sock, 1024)
        except OSError as exc:
            raise TransportError(f"RFCOMM read failed: {exc}") from exc
        if not data:
            raise TransportError("RFCOMM channel closed by the device")
        return data
