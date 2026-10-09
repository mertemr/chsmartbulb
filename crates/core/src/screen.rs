//! The `screen` effect: the light follows one colour that stands for a whole picture.
//!
//! Capturing is left to whoever has the screen (an agent, or a platform layer);
//! they push the reduced colour into a [`ScreenSource`].

use std::sync::{Arc, Mutex};

use crate::color::{rgb_to_hsv, round, Color, OFF};
use crate::effects::Effect;
use crate::error::{invalid, Result};

/// Red and blue, as fractions of the green value, that mix to the colour of the
/// bulb's white LEDs. Measured with a webcam on one unit; the green LEDs are by far the weakest.
const NEUTRAL: [f64; 3] = [0.48, 1.0, 0.23];
/// How much more a fully saturated pixel counts than a grey one.
const VIVID: f64 = 4.0;

/// Captures per second.
pub const RATE: f64 = 15.0;
/// Monitors are numbered from 1; 0 is all of them together.
pub const PRIMARY: i64 = 1;
/// Pixels kept along the shorter side of a picture.
pub const SAMPLES: u32 = 64;
/// Frames without a change before a capture starts to slow down (about a second).
pub const STILL_FRAMES: u32 = 15;
/// At most this many frame times between two captures of a picture that stands still.
pub const SLOWEST: u32 = 5;

/// How many frame times to wait before the next capture, after `still` unchanged frames.
pub fn pace(still: u32) -> u32 {
    (1 + still / STILL_FRAMES).min(SLOWEST)
}

/// Whether two colours differ by no more than the noise of a compressed picture.
pub fn similar(a: Color, b: Color) -> bool {
    const TOLERANCE: u8 = 2;
    a.r.abs_diff(b.r).max(a.g.abs_diff(b.g)).max(a.b.abs_diff(b.b)) <= TOLERANCE
}

/// The colour a screen shows, as last reported.
#[derive(Default)]
pub struct ScreenSource {
    color: Mutex<Color>,
}

impl ScreenSource {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn push(&self, color: Color) {
        *self.color.lock().unwrap_or_else(|p| p.into_inner()) = color;
    }

    /// Go dark, for when the feed stops.
    pub fn clear(&self) {
        self.push(OFF);
    }

    pub fn color(&self) -> Color {
        *self.color.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Reduce RGB pixels to one colour. A plain average tends towards grey, so saturated pixels weigh more.
pub fn picture_color(pixels: impl IntoIterator<Item = [u8; 3]>) -> Color {
    let (mut sum, mut total) = ([0.0f64; 3], 0.0f64);
    for [r, g, b] in pixels {
        let high = r.max(g).max(b) as f64;
        let low = r.min(g).min(b) as f64;
        let weight = 1.0 + VIVID * (high - low) / 255.0;
        for (sum, channel) in sum.iter_mut().zip([r, g, b]) {
            *sum += channel as f64 * weight;
        }
        total += weight;
    }
    if total == 0.0 {
        return OFF;
    }
    let channel = |value: f64| round(value / total).clamp(0.0, 255.0) as u8;
    Color::rgb(channel(sum[0]), channel(sum[1]), channel(sum[2]))
}

/// Weaken red and blue so that mixed colours keep their hue on the bulb.
///
/// The strongest channel stays where it was, so pure colours are not dimmed;
/// only the proportions change. `strength` 0 leaves the colour alone.
pub fn balanced(color: Color, strength: f64) -> Color {
    let top = color.r.max(color.g).max(color.b);
    if top == 0 || strength == 0.0 {
        return color;
    }
    let [r, g, b] = [color.r, color.g, color.b]
        .iter()
        .zip(NEUTRAL)
        .map(|(&channel, gain)| channel as f64 * gain.powf(strength))
        .collect::<Vec<_>>()
        .try_into()
        .expect("three channels");
    let scale = top as f64 / r.max(g).max(b);
    let channel = |value: f64| round(value * scale).clamp(0.0, 255.0) as u8;
    Color::rgbw(channel(r), channel(g), channel(b), color.w)
}

/// Show the colour of the screen.
///
/// `smoothing` is how many seconds the light takes to follow a change. `saturation`
/// multiplies the colourfulness. `white` (0..1) is how much of the grey goes to the
/// white LEDs. `balance` (0..1) corrects the rest for the weak green LEDs.
pub fn screen_follow(
    source: Arc<ScreenSource>,
    smoothing: f64,
    saturation: f64,
    white: f64,
    balance: f64,
) -> Result<Effect> {
    if smoothing < 0.0 {
        return Err(invalid("smoothing must not be negative"));
    }
    if saturation < 0.0 {
        return Err(invalid("saturation must not be negative"));
    }
    if !(0.0..=1.0).contains(&white) {
        return Err(invalid("white must be within 0..1"));
    }
    if !(0.0..=1.0).contains(&balance) {
        return Err(invalid("balance must be within 0..1"));
    }
    let mut shown = [0.0f64; 3];
    let mut last = 0.0;
    Ok(Box::new(move |t| {
        let step = (t - last).max(0.0);
        last = t;
        let blend = if smoothing > 0.0 { 1.0 - (-step / smoothing).exp() } else { 1.0 };
        let target = source.color();
        for (shown, channel) in shown.iter_mut().zip([target.r, target.g, target.b]) {
            *shown += (channel as f64 - *shown) * blend;
        }
        let (hue, colourfulness, value) = rgb_to_hsv(shown[0] / 255.0, shown[1] / 255.0, shown[2] / 255.0);
        let color = Color::from_hsv(hue * 360.0, (colourfulness * saturation).min(1.0), value);
        balanced(color.with_white(white), balance)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::RED;

    #[test]
    fn capture_slows_down_while_the_picture_stands_still() {
        assert_eq!(pace(0), 1);
        assert_eq!(pace(STILL_FRAMES - 1), 1);
        assert_eq!(pace(STILL_FRAMES), 2);
        assert_eq!(pace(10_000), SLOWEST); // never slower than that, however long it stands
        assert!(similar(Color::rgb(100, 100, 100), Color::rgb(101, 99, 102))); // compression noise is not movement
        assert!(!similar(Color::rgb(100, 100, 100), Color::rgb(100, 100, 110)));
    }

    #[test]
    fn balance_keeps_pure_colours() {
        assert_eq!(balanced(RED, 1.0), RED);
        assert_eq!(balanced(Color::rgb(255, 255, 255), 1.0), Color::rgb(122, 255, 59));
    }

    #[test]
    fn follow_reaches_the_screen_colour() {
        let source = ScreenSource::new();
        source.push(RED);
        let mut effect = screen_follow(source, 0.0, 1.0, 0.0, 0.0).unwrap();
        assert_eq!(effect(0.1), RED);
    }

    #[test]
    fn vivid_pixels_weigh_more() {
        let color = picture_color([[255, 0, 0], [128, 128, 128]]);
        assert!(color.r > 200, "{color:?}");
    }
}
