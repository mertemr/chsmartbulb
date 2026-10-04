"""Device-independent colour value used by every light and effect."""

from __future__ import annotations

import colorsys
from dataclasses import dataclass


def _clamp(value: float) -> int:
    return max(0, min(255, round(value)))


@dataclass(frozen=True)
class Color:
    """An RGB colour with an optional dedicated white channel, each 0..255."""

    r: int = 0
    g: int = 0
    b: int = 0
    w: int = 0

    def __post_init__(self) -> None:
        for name in ("r", "g", "b", "w"):
            value = getattr(self, name)
            if not isinstance(value, int) or not 0 <= value <= 255:
                raise ValueError(f"{name} must be an int in 0..255, got {value!r}")

    @classmethod
    def from_hex(cls, text: str) -> Color:
        """Parse ``rrggbb`` or ``rrggbbww`` (a leading ``#`` is allowed)."""
        digits = text.strip().lstrip("#")
        if len(digits) not in (6, 8):
            raise ValueError(f"expected 6 or 8 hex digits, got {text!r}")
        try:
            return cls(*bytes.fromhex(digits))
        except ValueError:
            raise ValueError(f"not a hex colour: {text!r}") from None

    @classmethod
    def from_hsv(cls, hue: float, saturation: float = 1.0, value: float = 1.0) -> Color:
        """Build an RGB colour from hue in degrees and saturation/value in 0..1."""
        r, g, b = colorsys.hsv_to_rgb((hue % 360) / 360, saturation, value)
        return cls(_clamp(r * 255), _clamp(g * 255), _clamp(b * 255))

    @property
    def is_off(self) -> bool:
        return not (self.r or self.g or self.b or self.w)

    def scaled(self, factor: float) -> Color:
        """Scale every channel; a lit channel never rounds down to dark unless factor is 0."""
        if factor <= 0:
            return OFF

        def scale(channel: int) -> int:
            return max(1, _clamp(channel * factor)) if channel else 0

        return Color(scale(self.r), scale(self.g), scale(self.b), scale(self.w))

    def mix(self, other: Color, t: float) -> Color:
        """Linear blend towards ``other``; ``t`` = 0 gives self, 1 gives other."""
        t = max(0.0, min(1.0, t))
        return Color(
            _clamp(self.r + (other.r - self.r) * t),
            _clamp(self.g + (other.g - self.g) * t),
            _clamp(self.b + (other.b - self.b) * t),
            _clamp(self.w + (other.w - self.w) * t),
        )

    def to_hex(self) -> str:
        return f"#{self.r:02x}{self.g:02x}{self.b:02x}" + (f"{self.w:02x}" if self.w else "")


# fmt: off
OFF     = Color()
RED     = Color(r=255)
GREEN   = Color(g=255)
BLUE    = Color(b=255)
YELLOW  = Color(r=255, g=255)
CYAN    = Color(g=255, b=255)
MAGENTA = Color(r=255, b=255)
WHITE   = Color(w=255)
# fmt: on

NAMED = {
    "off": OFF,
    "red": RED,
    "green": GREEN,
    "blue": BLUE,
    "yellow": YELLOW,
    "cyan": CYAN,
    "magenta": MAGENTA,
    "white": WHITE,
}


def parse_color(text: str) -> Color:
    """Accept a colour name or a hex string."""
    named = NAMED.get(text.strip().lower())
    return named if named is not None else Color.from_hex(text)
