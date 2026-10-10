//! The `screen` effects: the light follows one colour that stands for a whole picture,
//! alone or moved by the sound.
//!
//! Capturing is left to whoever has the screen (an agent, or a platform layer);
//! they push the reduced colour into a [`ScreenSource`].

use std::sync::{Arc, Mutex};

use crate::audio::AudioSource;
use crate::color::{rgb_to_hsv, round, Color, OFF};
use crate::effects::Effect;
use crate::error::{invalid, Result};

/// Red and blue, as fractions of the green value, that mix to the colour of the
/// bulb's white LEDs. Measured with a webcam on one unit; the green LEDs are by far the weakest.
const NEUTRAL: [f64; 3] = [0.48, 1.0, 0.23];
/// How much more a fully saturated pixel counts than a grey one.
const VIVID: f64 = 4.0;
/// The furthest the sound may turn the hue, in degrees: beyond it the screen's colour is lost.
pub const MAX_SHIFT: f64 = 60.0;
/// Seconds for the hue to settle on where the sound's weight lies.
const LEAN_SMOOTHING: f64 = 0.3;
const DARK: f64 = 1.0 / 255.0;

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

fn check(smoothing: f64, saturation: f64, white: f64, balance: f64) -> Result<()> {
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
    Ok(())
}

/// The screen's colour frame by frame, as hue (0..1), colourfulness and value.
fn followed(source: Arc<ScreenSource>, smoothing: f64) -> impl FnMut(f64) -> (f64, f64, f64) + Send {
    let mut shown = [0.0f64; 3];
    let mut last = 0.0;
    move |t| {
        let step = (t - last).max(0.0);
        last = t;
        let blend = if smoothing > 0.0 { 1.0 - (-step / smoothing).exp() } else { 1.0 };
        let target = source.color();
        for (shown, channel) in shown.iter_mut().zip([target.r, target.g, target.b]) {
            *shown += (channel as f64 - *shown) * blend;
        }
        rgb_to_hsv(shown[0] / 255.0, shown[1] / 255.0, shown[2] / 255.0)
    }
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
    check(smoothing, saturation, white, balance)?;
    let mut screen = followed(source, smoothing);
    Ok(Box::new(move |t| {
        let (hue, colourfulness, value) = screen(t);
        let color = Color::from_hsv(hue * 360.0, (colourfulness * saturation).min(1.0), value);
        balanced(color.with_white(white), balance)
    }))
}

/// How the sound moves the light of [`screen_sound`].
#[derive(Debug, Clone, Copy)]
pub struct Sway {
    /// The brightness (0..1) kept in silence.
    pub floor: f64,
    /// How fast the brightness falls after a loud moment, per second.
    pub release: f64,
    /// The most degrees the hue turns: one way for bass-heavy sound, the other for bright sound.
    pub shift: f64,
    /// Seconds the sound is held back, as for the sound effects.
    pub delay: f64,
}

/// Show the colour of the screen, as bright as the sound is loud.
///
/// The colour is that of [`screen_follow`]; `sway` is what the sound does to it.
pub fn screen_sound(
    screen: Arc<ScreenSource>,
    audio: Arc<AudioSource>,
    smoothing: f64,
    saturation: f64,
    white: f64,
    balance: f64,
    sway: Sway,
) -> Result<Effect> {
    check(smoothing, saturation, white, balance)?;
    let Sway { floor, release, shift, delay } = sway;
    if !(0.0..=1.0).contains(&floor) {
        return Err(invalid("floor must be within 0..1"));
    }
    if release <= 0.0 {
        return Err(invalid("release must be positive"));
    }
    if !(0.0..=MAX_SHIFT).contains(&shift) {
        return Err(invalid(format!("shift must be within 0..{MAX_SHIFT} degrees")));
    }
    audio.set_delay(delay)?;
    let mut screen = followed(screen, smoothing);
    let mut loud = 0.0f64;
    let mut lean = 0.0f64;
    let mut last = 0.0;
    Ok(Box::new(move |t| {
        let step = (t - last).max(0.0);
        last = t;
        let levels = audio.heard().levels;
        let loudest = levels.bass.max(levels.mid).max(levels.treble);
        loud = loudest.max(loud - release * step);
        // -1 all bass, +1 all treble; silence leans nowhere, so the screen's own colour comes back
        let total = levels.bass + levels.mid + levels.treble;
        let target = if total > 0.0 { (levels.treble - levels.bass) / total } else { 0.0 };
        lean += (target - lean) * (1.0 - (-step / LEAN_SMOOTHING).exp());
        let level = floor + (1.0 - floor) * loud * loud; // squared for contrast, as the sound effects do
        if level < DARK {
            return OFF; // scaled() never rounds a lit channel to zero
        }
        let (hue, colourfulness, value) = screen(t);
        let color = Color::from_hsv(hue * 360.0 + shift * lean, (colourfulness * saturation).min(1.0), value);
        balanced(color.with_white(white), balance).scaled(level)
    }))
}

/// How far a channel of the screen's colour jumps between two looks when the scene cuts.
const CUT: u8 = 48;

/// Show the colour of the screen, and flash the white LEDs when the scene cuts.
///
/// The colour is that of [`screen_follow`]. `flash` (0..1) is how bright the flash is,
/// `decay` how fast it dies away, per second.
pub fn screen_cut(
    source: Arc<ScreenSource>,
    smoothing: f64,
    saturation: f64,
    white: f64,
    balance: f64,
    flash: f64,
    decay: f64,
) -> Result<Effect> {
    check(smoothing, saturation, white, balance)?;
    if !(0.0..=1.0).contains(&flash) {
        return Err(invalid("flash must be within 0..1"));
    }
    if decay <= 0.0 {
        return Err(invalid("decay must be positive"));
    }
    let mut seen = source.color();
    let mut screen = followed(source.clone(), smoothing);
    let mut burst = 0.0f64;
    let mut last = 0.0;
    Ok(Box::new(move |t| {
        burst *= (-decay * (t - last).max(0.0)).exp();
        last = t;
        let now = source.color();
        if now.r.abs_diff(seen.r).max(now.g.abs_diff(seen.g)).max(now.b.abs_diff(seen.b)) >= CUT {
            burst = 1.0;
        }
        seen = now;
        let (hue, colourfulness, value) = screen(t);
        let color = Color::from_hsv(hue * 360.0, (colourfulness * saturation).min(1.0), value);
        let color = balanced(color.with_white(white), balance);
        let lift = round(255.0 * flash * burst) as u8;
        Color::rgbw(color.r, color.g, color.b, color.w.max(lift))
    }))
}

/// Below this colourfulness or value the screen has no hue worth taking.
const HUELESS: (f64, f64) = (0.15, 0.08);
/// The hue shown until the screen has had one: a warm orange.
const FIRST_HUE: f64 = 25.0 / 360.0;

/// Show only the hue of the screen, always fully lit: a dark or grey picture keeps the
/// last hue instead of dimming the room.
///
/// `saturation` (0..1) is how colourful the light is; the rest goes to the white LEDs.
pub fn screen_hue(source: Arc<ScreenSource>, smoothing: f64, saturation: f64, balance: f64) -> Result<Effect> {
    check(smoothing, saturation, 1.0, balance)?;
    if saturation > 1.0 {
        return Err(invalid("saturation must be within 0..1"));
    }
    let mut screen = followed(source, smoothing);
    let mut kept = FIRST_HUE;
    Ok(Box::new(move |t| {
        let (hue, colourfulness, value) = screen(t);
        if colourfulness >= HUELESS.0 && value >= HUELESS.1 {
            kept = hue;
        }
        balanced(Color::from_hsv(kept * 360.0, saturation, 1.0).with_white(1.0), balance)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{monotonic, Levels};
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

    fn sway(floor: f64, shift: f64) -> Sway {
        Sway { floor, release: 2.0, shift, delay: 0.0 }
    }

    #[test]
    fn sound_sets_the_brightness_of_the_screen_colour() {
        let (screen, audio) = (ScreenSource::new(), AudioSource::new(monotonic()));
        screen.push(RED);
        let mut effect = screen_sound(screen.clone(), audio.clone(), 0.0, 1.0, 0.0, 0.0, sway(0.2, 0.0)).unwrap();
        assert_eq!(effect(0.1), Color::rgb(51, 0, 0)); // silent: the floor
        audio.publish(Levels { mid: 1.0, ..Levels::default() }, 0.0);
        assert_eq!(effect(0.2), RED);
        audio.publish(Levels::default(), 0.0);
        assert_eq!(effect(0.45), Color::rgb(102, 0, 0)); // fallen to 0.5, squared, above the floor
        assert_eq!(effect(5.0), Color::rgb(51, 0, 0));
        let mut dark = screen_sound(screen, audio, 0.0, 1.0, 0.0, 0.0, sway(0.0, 0.0)).unwrap();
        assert_eq!(dark(0.1), OFF);
    }

    #[test]
    fn sound_turns_the_hue_a_little_either_way() {
        let (screen, audio) = (ScreenSource::new(), AudioSource::new(monotonic()));
        screen.push(Color::rgb(0, 255, 0));
        let mut effect = screen_sound(screen, audio.clone(), 0.0, 1.0, 0.0, 0.0, sway(1.0, 30.0)).unwrap();
        assert_eq!(effect(0.1), Color::rgb(0, 255, 0)); // silence leaves the colour alone
        audio.publish(Levels { treble: 1.0, ..Levels::default() }, 0.0);
        let bright = effect(10.0); // 30 degrees towards cyan
        assert!((bright.r, bright.g) == (0, 255) && (126..=129).contains(&bright.b), "{bright:?}");
        audio.publish(Levels { bass: 1.0, ..Levels::default() }, 0.0);
        let deep = effect(20.0); // and 30 towards yellow
        assert!((deep.g, deep.b) == (255, 0) && (126..=129).contains(&deep.r), "{deep:?}");
        audio.publish(Levels { bass: 1.0, treble: 1.0, ..Levels::default() }, 0.0);
        assert_eq!(effect(30.0), Color::rgb(0, 255, 0));
    }

    #[test]
    fn sound_refuses_what_does_not_fit() {
        let (screen, audio) = (ScreenSource::new(), AudioSource::new(monotonic()));
        for bad in [
            Sway { floor: 2.0, ..sway(0.3, 10.0) },
            Sway { release: 0.0, ..sway(0.3, 10.0) },
            Sway { shift: 90.0, ..sway(0.3, 10.0) },
            Sway { delay: 5.0, ..sway(0.3, 10.0) },
        ] {
            assert!(screen_sound(screen.clone(), audio.clone(), 0.2, 1.0, 1.0, 0.0, bad).is_err());
        }
        assert!(screen_sound(screen, audio, -1.0, 1.0, 1.0, 0.0, sway(0.3, 10.0)).is_err());
    }

    #[test]
    fn a_cut_flashes_the_white_leds() {
        let source = ScreenSource::new();
        source.push(RED);
        let mut effect = screen_cut(source.clone(), 0.0, 1.0, 0.0, 0.0, 0.5, 4.0).unwrap();
        assert_eq!(effect(0.1), RED);
        source.push(Color::rgb(255, 20, 0)); // the picture moving on is no cut
        assert_eq!(effect(0.2), Color::rgb(255, 20, 0));
        source.push(Color::rgb(0, 0, 255));
        assert_eq!(effect(0.3), Color::rgbw(0, 0, 255, 128));
        let fading = effect(0.55).w;
        assert!((40..=55).contains(&fading), "{fading}"); // a quarter second of decay 4 leaves e^-1
        assert_eq!(effect(10.0), Color::rgb(0, 0, 255));
        assert!(screen_cut(source.clone(), 0.0, 1.0, 0.0, 0.0, 2.0, 4.0).is_err());
        assert!(screen_cut(source, 0.0, 1.0, 0.0, 0.0, 0.5, 0.0).is_err());
    }

    #[test]
    fn hue_stays_lit_when_the_screen_goes_dark() {
        let source = ScreenSource::new();
        let mut effect = screen_hue(source.clone(), 0.0, 1.0, 0.0).unwrap();
        assert_eq!(effect(0.1), Color::rgb(255, 106, 0)); // nothing seen yet: the warm start
        source.push(Color::rgb(0, 40, 0)); // dim, and green all the same
        assert_eq!(effect(0.2), Color::rgb(0, 255, 0));
        source.push(OFF);
        assert_eq!(effect(0.3), Color::rgb(0, 255, 0));
        source.push(Color::rgb(200, 200, 205)); // grey has no hue to take
        assert_eq!(effect(0.4), Color::rgb(0, 255, 0));
        let mut pale = screen_hue(source.clone(), 0.0, 0.5, 0.0).unwrap();
        source.push(RED);
        assert_eq!(pale(0.1), Color::rgbw(127, 0, 0, 128));
        assert!(screen_hue(source, 0.0, 1.5, 0.0).is_err());
    }

    #[test]
    fn vivid_pixels_weigh_more() {
        let color = picture_color([[255, 0, 0], [128, 128, 128]]);
        assert!(color.r > 200, "{color:?}");
    }
}
