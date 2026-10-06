//! Device-independent colour value used by every light and effect.

use std::fmt;

use crate::error::{invalid, Result};

/// Round half to even, as Python's `round` does, so both implementations agree to the bit.
pub(crate) fn round(value: f64) -> f64 {
    value.round_ties_even()
}

fn clamp(value: f64) -> u8 {
    round(value).clamp(0.0, 255.0) as u8
}

/// An RGB colour with an optional dedicated white channel, each 0..255.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub w: u8,
}

pub const OFF: Color = Color::rgbw(0, 0, 0, 0);
pub const RED: Color = Color::rgbw(255, 0, 0, 0);
pub const GREEN: Color = Color::rgbw(0, 255, 0, 0);
pub const BLUE: Color = Color::rgbw(0, 0, 255, 0);
pub const YELLOW: Color = Color::rgbw(255, 255, 0, 0);
pub const CYAN: Color = Color::rgbw(0, 255, 255, 0);
pub const MAGENTA: Color = Color::rgbw(255, 0, 255, 0);
pub const WHITE: Color = Color::rgbw(0, 0, 0, 255);

pub const NAMED: [(&str, Color); 8] = [
    ("off", OFF),
    ("red", RED),
    ("green", GREEN),
    ("blue", BLUE),
    ("yellow", YELLOW),
    ("cyan", CYAN),
    ("magenta", MAGENTA),
    ("white", WHITE),
];

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, w: 0 }
    }

    pub const fn rgbw(r: u8, g: u8, b: u8, w: u8) -> Self {
        Self { r, g, b, w }
    }

    /// Parse `rrggbb` or `rrggbbww` (a leading `#` is allowed).
    pub fn from_hex(text: &str) -> Result<Self> {
        let digits = text.trim().trim_start_matches('#');
        if digits.len() != 6 && digits.len() != 8 {
            return Err(invalid(format!("expected 6 or 8 hex digits, got {text:?}")));
        }
        let mut bytes = [0u8; 4];
        for (i, byte) in bytes.iter_mut().take(digits.len() / 2).enumerate() {
            *byte = digits
                .get(2 * i..2 * i + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .ok_or_else(|| invalid(format!("not a hex colour: {text:?}")))?;
        }
        Ok(Self::rgbw(bytes[0], bytes[1], bytes[2], bytes[3]))
    }

    /// Build an RGB colour from hue in degrees and saturation/value in 0..1.
    pub fn from_hsv(hue: f64, saturation: f64, value: f64) -> Self {
        let (r, g, b) = hsv_to_rgb(hue.rem_euclid(360.0) / 360.0, saturation, value);
        Self::rgb(clamp(r * 255.0), clamp(g * 255.0), clamp(b * 255.0))
    }

    pub fn is_off(&self) -> bool {
        self.r == 0 && self.g == 0 && self.b == 0 && self.w == 0
    }

    /// Scale every channel; a lit channel never rounds down to dark unless factor is 0.
    pub fn scaled(&self, factor: f64) -> Self {
        if factor <= 0.0 {
            return OFF;
        }
        let scale = |channel: u8| if channel == 0 { 0 } else { clamp(channel as f64 * factor).max(1) };
        Self::rgbw(scale(self.r), scale(self.g), scale(self.b), scale(self.w))
    }

    /// Move the grey part of the red, green and blue mix to the white channel.
    ///
    /// Equal red, green and blue rarely make a neutral white on an RGBW light;
    /// its white LEDs do. `share` (0..1) is how much of the grey part moves.
    pub fn with_white(&self, share: f64) -> Self {
        let lowest = self.r.min(self.g).min(self.b) as f64;
        let grey = round(lowest * share.clamp(0.0, 1.0)) as u8;
        Self::rgbw(self.r - grey, self.g - grey, self.b - grey, self.w.saturating_add(grey))
    }

    /// Linear blend towards `other`; `t` = 0 gives self, 1 gives other.
    pub fn mix(&self, other: &Color, t: f64) -> Self {
        let t = t.clamp(0.0, 1.0);
        let blend = |a: u8, b: u8| clamp(a as f64 + (b as f64 - a as f64) * t);
        Self::rgbw(blend(self.r, other.r), blend(self.g, other.g), blend(self.b, other.b), blend(self.w, other.w))
    }

    pub fn to_hex(&self) -> String {
        let mut text = format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b);
        if self.w != 0 {
            text.push_str(&format!("{:02x}", self.w));
        }
        text
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Accept a colour name or a hex string.
pub fn parse_color(text: &str) -> Result<Color> {
    let name = text.trim().to_lowercase();
    match NAMED.iter().find(|(known, _)| *known == name) {
        Some((_, color)) => Ok(*color),
        None => Color::from_hex(text),
    }
}

/// Python's `colorsys.hsv_to_rgb`, everything in 0..1.
pub(crate) fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (f64, f64, f64) {
    if s == 0.0 {
        return (v, v, v);
    }
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match (i as i64).rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

/// Python's `colorsys.rgb_to_hsv`, everything in 0..1.
pub(crate) fn rgb_to_hsv(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let high = r.max(g).max(b);
    let low = r.min(g).min(b);
    let v = high;
    if high == low {
        return (0.0, 0.0, v);
    }
    let range = high - low;
    let s = range / high;
    let rc = (high - r) / range;
    let gc = (high - g) / range;
    let bc = (high - b) / range;
    let h = if r == high {
        bc - gc
    } else if g == high {
        2.0 + rc - bc
    } else {
        4.0 + gc - rc
    };
    ((h / 6.0).rem_euclid(1.0), s, v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        assert_eq!(Color::from_hex("#ff0064").unwrap(), Color::rgb(255, 0, 100));
        assert_eq!(Color::from_hex("102030ff").unwrap(), Color::rgbw(16, 32, 48, 255));
        assert_eq!(Color::rgbw(16, 32, 48, 255).to_hex(), "#102030ff");
        assert_eq!(RED.to_hex(), "#ff0000");
        assert!(Color::from_hex("#12345").is_err());
        assert!(Color::from_hex("zz0000").is_err());
        assert_eq!(parse_color(" White ").unwrap(), WHITE);
    }

    #[test]
    fn scaling_keeps_lit_channels_lit() {
        assert_eq!(Color::rgb(255, 1, 0).scaled(0.1), Color::rgb(26, 1, 0));
        assert_eq!(RED.scaled(0.0), OFF);
        // half to even, like Python
        assert_eq!(Color::rgb(5, 0, 0).scaled(0.5), Color::rgb(2, 0, 0));
    }

    #[test]
    fn hsv_matches_colorsys() {
        assert_eq!(Color::from_hsv(0.0, 1.0, 1.0), RED);
        assert_eq!(Color::from_hsv(120.0, 1.0, 1.0), GREEN);
        assert_eq!(Color::from_hsv(480.0, 1.0, 1.0), GREEN);
        let (h, s, v) = rgb_to_hsv(0.0, 0.0, 1.0);
        assert!((h - 2.0 / 3.0).abs() < 1e-12 && s == 1.0 && v == 1.0);
    }

    #[test]
    fn white_and_mix() {
        assert_eq!(Color::rgb(200, 100, 50).with_white(1.0), Color::rgbw(150, 50, 0, 50));
        assert_eq!(OFF.mix(&Color::rgb(255, 100, 0), 0.5), Color::rgb(128, 50, 0));
    }
}
