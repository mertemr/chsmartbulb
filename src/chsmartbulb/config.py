"""Settings kept in a file, for what would otherwise be typed or exported every time.

The file holds ``CHSMARTBULB_ADDRESS=AA:BB:CC:DD:EE:FF`` lines under the names the environment
uses. The environment overrides the file, and the command line overrides both.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path
from typing import TYPE_CHECKING

from .errors import SmartBulbError

if TYPE_CHECKING:
    from collections.abc import Mapping

ADDRESS = "CHSMARTBULB_ADDRESS"
TOKEN = "CHSMARTBULB_TOKEN"
HOST = "CHSMARTBULB_HOST"
TRANSPORT = "CHSMARTBULB_TRANSPORT"
AUDIO_DEVICE = "CHSMARTBULB_AUDIO_DEVICE"
MONITOR = "CHSMARTBULB_MONITOR"
NAMES = (ADDRESS, TOKEN, HOST, TRANSPORT, AUDIO_DEVICE, MONITOR)


def default_path() -> Path:
    if sys.platform == "win32":
        roaming = os.environ.get("APPDATA")
        base = Path(roaming) if roaming else Path.home() / "AppData" / "Roaming"
    else:
        config = os.environ.get("XDG_CONFIG_HOME")
        base = Path(config) if config else Path.home() / ".config"
    return base / "chsmartbulb" / "config"


def parse(text: str, source: str = "settings") -> dict[str, str]:
    """The settings in ``text``; ``source`` names it in the error for a line that sets none of them."""
    values: dict[str, str] = {}
    for number, raw in enumerate(text.splitlines(), start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        name, separator, value = line.removeprefix("export ").partition("=")
        name, value = name.strip(), value.strip()
        if not separator or name not in NAMES:
            raise SmartBulbError(f"{source}:{number}: expected one of {', '.join(NAMES)} followed by =VALUE")
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        values[name] = value
    return values


def load(path: Path | None = None, environ: Mapping[str, str] | None = None) -> dict[str, str]:
    """The settings in force: the file's, overridden by the environment's. Empty values count as unset."""
    path = default_path() if path is None else path
    environ = os.environ if environ is None else environ
    try:
        values = parse(path.read_text(encoding="utf-8-sig"), str(path))  # Notepad puts a mark at the start
    except FileNotFoundError:
        values = {}
    except OSError as exc:
        raise SmartBulbError(f"cannot read {path}: {exc.strerror or exc}") from exc
    values.update({name: environ[name] for name in NAMES if name in environ})
    return {name: value for name, value in values.items() if value}
