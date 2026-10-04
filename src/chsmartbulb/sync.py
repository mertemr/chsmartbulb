"""Blocking facade for scripts and the REPL."""

from __future__ import annotations

import asyncio
import inspect
import threading
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from typing_extensions import Self


class BlockingLight:
    """Wraps an async light so its coroutine methods can be called like plain functions.

    A private event loop runs in a daemon thread; every call is handed to it and
    waited for, so effects started with :meth:`start` keep running between calls.

        bulb = BlockingLight(ChSmartBulb.rfcomm("AA:BB:CC:DD:EE:FF"))
        bulb.connect()
        bulb.set_rgb(255, 0, 100)
        bulb.disconnect()
    """

    def __init__(self, light: Any) -> None:
        self._light = light
        self._loop = asyncio.new_event_loop()
        self._thread = threading.Thread(target=self._loop.run_forever, daemon=True)
        self._thread.start()

    def run(self, coroutine: Any) -> Any:
        """Run any coroutine on the light's loop and return its result."""
        return asyncio.run_coroutine_threadsafe(coroutine, self._loop).result()

    def __getattr__(self, name: str) -> Any:
        attribute = getattr(self._light, name)
        if not inspect.iscoroutinefunction(attribute):
            return attribute

        def call(*args: Any, **kwargs: Any) -> Any:
            return self.run(attribute(*args, **kwargs))

        return call

    def __enter__(self) -> Self:
        self.connect()
        return self

    def __exit__(self, *exc: object) -> None:
        self.disconnect()
