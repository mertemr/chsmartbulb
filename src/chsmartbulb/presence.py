"""The computer being locked, asleep or shut down, as events (Windows only).

A hidden window on its own thread receives what Windows tells every window: the session being
locked or unlocked, the machine going to sleep or waking, the session ending. Only the mapping to
events (:func:`event_of`) is platform-independent.
"""

from __future__ import annotations

import asyncio
import logging
import sys
import threading
from typing import TYPE_CHECKING, Any

from .errors import SmartBulbError

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping

log = logging.getLogger(__name__)

WM_CLOSE = 0x0010
WM_DESTROY = 0x0002
WM_QUERYENDSESSION = 0x0011
WM_ENDSESSION = 0x0016
WM_POWERBROADCAST = 0x0218
WM_WTSSESSION_CHANGE = 0x02B1

_WTS_SESSION_LOCK = 7
_WTS_SESSION_UNLOCK = 8
_PBT_APMSUSPEND = 4
_PBT_APMRESUMESUSPEND = 7
_PBT_APMRESUMEAUTOMATIC = 0x12
_NOTIFY_FOR_THIS_SESSION = 0


def event_of(message: int, wparam: int) -> str | None:
    """``lock``, ``unlock``, ``sleep``, ``resume`` or ``shutdown`` for a window message, else ``None``."""
    if message == WM_WTSSESSION_CHANGE:
        return {_WTS_SESSION_LOCK: "lock", _WTS_SESSION_UNLOCK: "unlock"}.get(wparam)
    if message == WM_POWERBROADCAST:
        return {_PBT_APMSUSPEND: "sleep", _PBT_APMRESUMESUSPEND: "resume", _PBT_APMRESUMEAUTOMATIC: "resume"}.get(
            wparam
        )
    if message == WM_ENDSESSION and wparam:  # zero: the shutdown was cancelled
        return "shutdown"
    return None


def request_of(event: str) -> Mapping[str, Any]:
    """The service request that goes with an event."""
    return {"cmd": "back"} if event in ("unlock", "resume") else {"cmd": "away", "reason": event}


class PresenceWatcher:
    """Calls ``on_event`` on the event loop that started it, for each of :func:`event_of`'s events."""

    def __init__(self, on_event: Callable[[str], None]) -> None:
        if sys.platform != "win32":
            raise SmartBulbError("following whether the computer is locked or asleep needs Windows")
        self._on_event = on_event
        self._window: int | None = None
        self._thread: threading.Thread | None = None

    async def start(self) -> None:
        loop = asyncio.get_running_loop()
        ready: asyncio.Future[None] = loop.create_future()
        self._thread = threading.Thread(target=self._run, args=(loop, ready), name="chsmartbulb-presence", daemon=True)
        self._thread.start()
        await ready

    async def stop(self) -> None:
        window, self._window = self._window, None
        if window is not None:
            import ctypes

            ctypes.windll.user32.PostMessageW(window, WM_CLOSE, 0, 0)
        thread, self._thread = self._thread, None
        if thread is not None:
            await asyncio.get_running_loop().run_in_executor(None, thread.join, 2.0)

    def _run(self, loop: asyncio.AbstractEventLoop, ready: asyncio.Future[None]) -> None:
        import ctypes
        from ctypes import wintypes

        user32, wtsapi32, kernel32 = ctypes.windll.user32, ctypes.windll.wtsapi32, ctypes.windll.kernel32
        wndproc_type = ctypes.WINFUNCTYPE(
            ctypes.c_ssize_t, wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM
        )
        user32.DefWindowProcW.restype = ctypes.c_ssize_t
        user32.DefWindowProcW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
        user32.CreateWindowExW.restype = wintypes.HWND
        user32.CreateWindowExW.argtypes = [
            wintypes.DWORD, wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.DWORD,
            ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int,
            wintypes.HWND, wintypes.HMENU, wintypes.HINSTANCE, wintypes.LPVOID,
        ]  # fmt: skip
        user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]

        def procedure(window: int, message: int, wparam: int, lparam: int) -> int:
            if message == WM_QUERYENDSESSION:
                return 1  # never hold up a shutdown
            if message == WM_DESTROY:
                user32.PostQuitMessage(0)
                return 0
            if event := event_of(message, wparam):
                log.debug("presence: %s", event)
                with _quiet(RuntimeError):  # the loop is gone while shutting down
                    loop.call_soon_threadsafe(self._on_event, event)
            return user32.DefWindowProcW(window, message, wparam, lparam)

        class WindowClass(ctypes.Structure):
            _fields_ = [
                ("style", wintypes.UINT),
                ("lpfnWndProc", wndproc_type),
                ("cbClsExtra", ctypes.c_int),
                ("cbWndExtra", ctypes.c_int),
                ("hInstance", wintypes.HINSTANCE),
                ("hIcon", wintypes.HANDLE),
                ("hCursor", wintypes.HANDLE),
                ("hbrBackground", wintypes.HANDLE),
                ("lpszMenuName", wintypes.LPCWSTR),
                ("lpszClassName", wintypes.LPCWSTR),
            ]

        keep = wndproc_type(procedure)  # must outlive the window
        instance = kernel32.GetModuleHandleW(None)
        name = "chsmartbulb.presence"
        definition = WindowClass(0, keep, 0, 0, instance, None, None, None, None, name)
        user32.RegisterClassW.argtypes = [ctypes.POINTER(WindowClass)]
        user32.RegisterClassW(ctypes.byref(definition))  # registering twice only fails; the old class serves
        # a top-level window that is never shown: message-only windows are not told about power or shutdown
        window = user32.CreateWindowExW(0, name, name, 0, 0, 0, 0, 0, None, None, instance, None)
        if not window:
            failure = SmartBulbError(f"cannot create the window for session events: error {ctypes.GetLastError()}")
            loop.call_soon_threadsafe(ready.set_exception, failure)
            return
        wtsapi32.WTSRegisterSessionNotification(wintypes.HWND(window), _NOTIFY_FOR_THIS_SESSION)
        self._window = window
        loop.call_soon_threadsafe(ready.set_result, None)
        message = wintypes.MSG()
        while user32.GetMessageW(ctypes.byref(message), None, 0, 0) > 0:
            user32.TranslateMessage(ctypes.byref(message))
            user32.DispatchMessageW(ctypes.byref(message))
        wtsapi32.WTSUnRegisterSessionNotification(wintypes.HWND(window))


def _quiet(*errors: type[BaseException]) -> Any:
    import contextlib

    return contextlib.suppress(*errors)
