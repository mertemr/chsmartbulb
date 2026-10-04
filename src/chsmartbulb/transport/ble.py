"""Bluetooth Low Energy GATT pipe: write one characteristic, get notified on another.

Needs the optional ``bleak`` dependency (``pip install chsmartbulb[ble]``).
"""

from __future__ import annotations

import asyncio
from typing import Any

from ..errors import ConnectionFailed, TransportError
from .base import Transport


class BleTransport(Transport):
    """Writes with response to ``write_uuid`` and receives from ``notify_uuid``.

    ``address`` may also be a ``bleak.BLEDevice`` (for example from a scan),
    which lets bleak skip its own scan.
    """

    def __init__(
        self,
        address: Any,
        write_uuid: str,
        notify_uuid: str,
        *,
        connect_timeout: float = 20.0,
    ) -> None:
        self.address = address
        self._write_uuid = write_uuid
        self._notify_uuid = notify_uuid
        self._connect_timeout = connect_timeout
        self._client = None
        self._incoming: asyncio.Queue[bytes | None] = asyncio.Queue()

    @property
    def is_open(self) -> bool:
        return self._client is not None and self._client.is_connected

    async def open(self) -> None:
        if self.is_open:
            return
        try:
            from bleak import BleakClient
            from bleak.exc import BleakError
        except ImportError:
            raise ConnectionFailed("BLE needs the 'bleak' package: pip install chsmartbulb[ble]") from None
        self._incoming = asyncio.Queue()
        incoming = self._incoming
        client = BleakClient(
            self.address,
            timeout=self._connect_timeout,
            disconnected_callback=lambda _client: incoming.put_nowait(None),
        )
        try:
            await client.connect()
            await client.start_notify(self._notify_uuid, lambda _char, data: incoming.put_nowait(bytes(data)))
            # BlueZ reports "connected" even when it brought up the Classic link of a
            # dual-mode device instead of LE; a GATT read exposes that right away.
            await client.read_gatt_char(self._write_uuid)
        except (BleakError, asyncio.TimeoutError, OSError) as exc:
            try:
                await client.disconnect()
            except (BleakError, OSError):
                pass
            raise ConnectionFailed(f"cannot connect to {self.address} over BLE: {exc}") from exc
        self._client = client

    async def close(self) -> None:
        client, self._client = self._client, None
        if client is not None:
            try:
                await client.disconnect()
            except Exception:  # bleak backends raise assorted errors on a dead link
                pass

    async def write(self, data: bytes) -> None:
        client = self._client
        if client is None or not client.is_connected:
            raise TransportError("BLE link is closed")
        try:
            # The bulb ignores write-without-response, so always ask for one.
            await client.write_gatt_char(self._write_uuid, data, response=True)
        except Exception as exc:
            raise TransportError(f"BLE write failed: {exc}") from exc

    async def read(self) -> bytes:
        data = await self._incoming.get()
        if data is None:
            raise TransportError("BLE link closed by the device")
        return data
