"""The Rust core, as ``chsmartbulb-native`` brings it to Python."""

from __future__ import annotations

import os
from typing import Any

from .errors import SmartBulbError


def load() -> Any:
    """The ``chsmartbulb_native`` module when it is installed, else ``None``.

    ``CHSMARTBULB_NATIVE=0`` makes the package do without it.
    """
    if os.environ.get("CHSMARTBULB_NATIVE", "1") == "0":
        return None
    try:
        import chsmartbulb_native
    except ImportError:
        return None
    return chsmartbulb_native


def require(what: str) -> Any:
    """The module, or an error saying that ``what`` cannot do without it."""
    module = load()
    if module is None:
        raise SmartBulbError(
            f"{what} needs chsmartbulb-native, the Rust core for Python: pip install "
            '"chsmartbulb-native @ git+https://github.com/mertemr/chsmartbulb#subdirectory=crates/python"'
        )
    return module
