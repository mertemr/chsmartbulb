//! Host-driven effects: colours computed on the host and streamed to the bulb.
//!
//! An effect is a function from elapsed seconds to a [`Color`]. Some keep a little
//! state between frames (how far a level has fallen), so they are `FnMut`.

use std::f64::consts::PI;

use serde_json::Value;

use crate::color::{parse_color, Color, BLUE, OFF, RED};
use crate::error::{invalid, Result};

pub type Effect = Box<dyn FnMut(f64) -> Color + Send>;

/// The CHSmartBulb follows about 25 colour changes per second; 20 leaves headroom.
pub const DEFAULT_FPS: f64 = 20.0;
/// One BLE write takes about 75 ms, so over BLE the bulb takes about 13 a second.
pub const BLE_FPS: f64 = 12.0;

pub const WARM: Color = Color::rgb(255, 110, 20);

/// Names of the curves a fade can follow, in the order front ends offer them.
pub const EASINGS: [&str; 4] = ["linear", "ease-in", "ease-out", "ease-in-out"];
/// The most steps a custom effect takes.
pub const MAX_STEPS: usize = 16;

/// How a fade moves from 0 to 1 as its time runs from 0 to 1.
pub fn ease(name: &str) -> Option<fn(f64) -> f64> {
    Some(match name {
        "linear" => |x| x,
        "ease-in" => |x| x * x,
        "ease-out" => |x| 1.0 - (1.0 - x) * (1.0 - x),
        "ease-in-out" => |x| x * x * (3.0 - 2.0 * x),
        _ => return None,
    })
}

pub fn solid(color: Color) -> Effect {
    Box::new(move |_| color)
}

/// Swell between `floor` (0..1) and full brightness, starting dark.
pub fn breathe(color: Color, period: f64, floor: f64) -> Effect {
    Box::new(move |t| {
        let level = (1.0 - (2.0 * PI * t / period).cos()) / 2.0;
        color.scaled(floor + (1.0 - floor) * level * level) // squared: gentler at the dim end
    })
}

/// Walk once around the colour wheel every `period` seconds.
pub fn hue_cycle(period: f64, saturation: f64, value: f64) -> Effect {
    Box::new(move |t| Color::from_hsv(360.0 * t / period, saturation, value))
}

/// Flash to `color` at the start of every period, then decay towards `base`.
pub fn pulse(color: Color, period: f64, decay: f64, base: Color) -> Effect {
    Box::new(move |t| {
        let phase = t.rem_euclid(period) / period;
        base.mix(&color, (-decay * phase).exp())
    })
}

/// Hard on/off blinking.
pub fn strobe(color: Color, hz: f64, duty: f64, base: Color) -> Effect {
    Box::new(move |t| if (t * hz).rem_euclid(1.0) < duty { color } else { base })
}

/// Uneven flicker: `depth` is how far the flame dips, 0 (steady) to 1 (to dark).
pub fn candle(color: Color, depth: f64) -> Effect {
    Box::new(move |t| {
        // three sines with unrelated frequencies never line up, which reads as random
        let wobble = ((7.3 * t).sin() + (12.1 * t + 1.3).sin() + (23.7 * t + 0.5).sin()) / 3.0;
        color.scaled(1.0 - depth * (0.5 + 0.5 * wobble))
    })
}

/// Scale another effect's output.
pub fn dimmed(mut effect: Effect, level: f64) -> Effect {
    Box::new(move |t| effect(t).scaled(level))
}

/// One step of a [`sequence`]: blend in over `fade_in` seconds, shaped by `ease`, then hold.
#[derive(Debug, Clone)]
pub struct Step {
    pub color: Color,
    pub hold: f64,
    pub fade_in: f64,
    pub ease: String,
}

impl Step {
    pub fn new(color: Color, hold: f64, fade_in: f64) -> Self {
        Self { color, hold, fade_in, ease: "linear".into() }
    }
}

/// A custom colour sequence with custom timing.
///
/// The light blends from the previous step's colour over `fade_in` seconds,
/// then holds for `hold` seconds.
pub fn sequence(steps: Vec<Step>, looping: bool) -> Result<Effect> {
    if steps.is_empty() {
        return Err(invalid("sequence needs at least one step"));
    }
    let mut normalized = Vec::with_capacity(steps.len());
    for step in steps {
        let curve = ease(&step.ease)
            .ok_or_else(|| invalid(format!("unknown easing {:?}; choose from: {}", step.ease, EASINGS.join(", "))))?;
        normalized.push((step.color, step.hold, step.fade_in, curve));
    }
    let total: f64 = normalized.iter().map(|(_, hold, fade_in, _)| hold + fade_in).sum();
    if total <= 0.0 {
        return Err(invalid("sequence must last longer than zero seconds"));
    }
    let last = normalized[normalized.len() - 1].0;
    Ok(Box::new(move |t| {
        let mut t = t;
        if looping {
            t = t.rem_euclid(total);
        } else if t >= total {
            return last;
        }
        // before the first lap completes there is no previous colour to blend from
        let mut previous = if looping { last } else { normalized[0].0 };
        for &(color, hold, fade_in, curve) in &normalized {
            if t < fade_in {
                return previous.mix(&color, curve(t / fade_in));
            }
            t -= fade_in;
            if t < hold {
                return color;
            }
            t -= hold;
            previous = color;
        }
        last
    }))
}

/// Drift through `colors` in order, holding each and blending into the next.
pub fn palette(colors: &[Color], hold: f64, fade_in: f64) -> Result<Effect> {
    sequence(colors.iter().map(|&color| Step::new(color, hold, fade_in)).collect(), true)
}

/// Alternate red and blue.
pub fn police(period: f64) -> Result<Effect> {
    sequence(vec![Step::new(RED, period / 2.0, 0.0), Step::new(BLUE, period / 2.0, 0.0)], true)
}

const STEP_KEYS: [&str; 4] = ["color", "hold", "fade", "ease"];

fn number(value: &Value, what: &str) -> Result<f64> {
    match value {
        Value::Number(n) => n.as_f64().ok_or_else(|| invalid(format!("{what} must be a number"))),
        Value::String(s) => s.trim().parse().map_err(|_| invalid(format!("{what} must be a number, got {s:?}"))),
        other => Err(invalid(format!("{what} must be a number, got {other}"))),
    }
}

/// A looping sequence described with plain data, as a front end or a file would hold it.
///
/// Each step is `{"color": ..., "hold": seconds, "fade": seconds, "ease": name}`;
/// only the colour is required. `speed` multiplies the pace of the whole sequence.
pub fn custom(steps: &[Value], speed: f64) -> Result<Effect> {
    if speed <= 0.0 {
        return Err(invalid("speed must be positive"));
    }
    if steps.len() > MAX_STEPS {
        return Err(invalid(format!("a custom effect takes at most {MAX_STEPS} steps")));
    }
    let mut parsed = Vec::with_capacity(steps.len());
    for (number_, step) in steps.iter().enumerate() {
        let number_ = number_ + 1;
        let Value::Object(step) = step else {
            return Err(invalid(format!("step {number_} must be an object with a color")));
        };
        if let Some(key) = step.keys().find(|key| !STEP_KEYS.contains(&key.as_str())) {
            return Err(invalid(format!("step {number_} has no {key:?}; it takes: {}", STEP_KEYS.join(", "))));
        }
        let color = match step.get("color") {
            None | Some(Value::Null) => return Err(invalid(format!("step {number_} needs a color"))),
            Some(Value::String(text)) => parse_color(text)?,
            Some(other) => parse_color(&other.to_string())?,
        };
        let hold = step.get("hold").map(|v| number(v, "hold")).transpose()?.unwrap_or(1.0);
        let fade_in = step.get("fade").map(|v| number(v, "fade")).transpose()?.unwrap_or(0.0);
        if hold < 0.0 || fade_in < 0.0 {
            return Err(invalid(format!("hold and fade of step {number_} must not be negative")));
        }
        let ease = match step.get("ease") {
            None => "linear".to_string(),
            Some(Value::String(name)) => name.clone(),
            Some(other) => other.to_string(),
        };
        parsed.push(Step { color, hold, fade_in, ease });
    }
    let mut played = sequence(parsed, true)?;
    Ok(Box::new(move |t| played(t * speed)))
}

/// The colour `x` (0..1) of the way along `colors`, blending between neighbours.
pub(crate) fn along(colors: &[Color], x: f64) -> Color {
    let Some(&last) = colors.last() else { return OFF };
    let place = x.clamp(0.0, 1.0) * (colors.len() - 1) as f64;
    let index = place.floor() as usize;
    match colors.get(index + 1) {
        Some(next) => colors[index].mix(next, place - index as f64),
        None => last,
    }
}

/// Slow curtains of light wandering back and forth through `colors`.
///
/// `period` is roughly how long one wander takes; `depth` (0..1) is how far the light dims
/// as it goes.
pub fn aurora(colors: &[Color], period: f64, depth: f64) -> Result<Effect> {
    if colors.is_empty() {
        return Err(invalid("colors must not be empty"));
    }
    if period <= 0.0 {
        return Err(invalid("period must be positive"));
    }
    if !(0.0..=1.0).contains(&depth) {
        return Err(invalid("depth must be within 0..1"));
    }
    let colors = colors.to_vec();
    Ok(Box::new(move |t| {
        // sines with unrelated frequencies, as the candle uses: the drift never quite repeats
        let a = 2.0 * PI * t / period;
        let place = 0.5 + 0.3 * a.sin() + 0.2 * (0.37 * a + 1.7).sin();
        let dip = 0.5 + 0.3 * (2.3 * a + 0.4).sin() + 0.2 * (5.1 * a).sin();
        along(&colors, place).scaled(1.0 - depth * dip)
    }))
}

/// A fire: glowing `embers` that flare up into `flame`, never the same twice.
///
/// `depth` (0..1) is how far it dies down between flares, `speed` multiplies its pace.
pub fn fire(embers: Color, flame: Color, depth: f64, speed: f64) -> Result<Effect> {
    if !(0.0..=1.0).contains(&depth) {
        return Err(invalid("depth must be within 0..1"));
    }
    if speed <= 0.0 {
        return Err(invalid("speed must be positive"));
    }
    Ok(Box::new(move |t| {
        let t = t * speed;
        let flicker = ((9.1 * t).sin() + (15.7 * t + 1.3).sin() + (31.3 * t + 0.5).sin()) / 3.0;
        let swell = (1.3 * t).sin() * (0.7 * t + 2.1).sin(); // the slow rise and fall of the whole fire
        let heat = (0.5 + 0.3 * swell + 0.2 * flicker).clamp(0.0, 1.0);
        embers.mix(&flame, heat * heat).scaled(1.0 - depth * (1.0 - heat))
    }))
}

/// How fast a stroke of lightning fades, per second.
const BOLT_DECAY: f64 = 14.0;
/// The most strokes in one strike.
const MAX_STROKES: usize = 4;

/// Numbers that look random and are the same on every run: xorshift, 0..1.
struct Dice(u64);

impl Dice {
    fn roll(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// A thunderstorm: a dim `sky`, and strikes of `bolt` that flicker a few times and die away.
///
/// `glow` (0..1) is how bright the sky stays between strikes and `power` (0..1) how bright
/// a strike is. Strikes come `gap` seconds apart on average, at uneven intervals.
pub fn lightning(bolt: Color, sky: Color, glow: f64, power: f64, gap: f64) -> Result<Effect> {
    if !(0.0..=1.0).contains(&glow) {
        return Err(invalid("glow must be within 0..1"));
    }
    if !(0.0..=1.0).contains(&power) {
        return Err(invalid("power must be within 0..1"));
    }
    if gap <= 0.0 {
        return Err(invalid("gap must be positive"));
    }
    let sky = sky.scaled(glow);
    let mut dice = Dice(0x9E37_79B9_7F4A_7C15);
    let mut next = gap * 0.3; // the first strike comes soon, to show what was chosen
    let mut strokes: Vec<(f64, f64)> = Vec::with_capacity(MAX_STROKES); // when, how strong
    Ok(Box::new(move |t| {
        if t >= next {
            strokes.clear();
            let count = 1 + (dice.roll() * MAX_STROKES as f64) as usize;
            let mut at = t;
            for stroke in 0..count.min(MAX_STROKES) {
                // the first stroke is the full one, the echoes are weaker
                strokes.push((at, if stroke == 0 { 1.0 } else { 0.4 + 0.5 * dice.roll() }));
                at += 0.06 + 0.12 * dice.roll();
            }
            next = at + gap * (0.3 + 1.4 * dice.roll());
        }
        let level = strokes
            .iter()
            .filter(|(at, _)| t >= *at)
            .map(|(at, strength)| strength * (-BOLT_DECAY * (t - at)).exp())
            .fold(0.0, f64::max);
        sky.mix(&bolt, level * power)
    }))
}

/// Where a sunrise starts and what it passes on the way to its last colour.
const DAWN: [Color; 2] = [Color::rgb(255, 20, 0), Color::rgb(255, 90, 0)];
/// The light a sunrise ends on: warm, with the white LEDs.
pub const MORNING: Color = Color::rgbw(255, 150, 40, 255);

/// A sunrise: from dark through deep red and orange to `color`, over `minutes`, then it stays.
pub fn sunrise(color: Color, minutes: f64) -> Result<Effect> {
    if minutes <= 0.0 {
        return Err(invalid("minutes must be positive"));
    }
    let stages = [DAWN[0], DAWN[1], color];
    Ok(Box::new(move |t| {
        let risen = (t / (minutes * 60.0)).clamp(0.0, 1.0);
        // squared: the eye takes the first light for more than it is
        along(&stages, risen).scaled(risen * risen)
    }))
}

/// Black, for effects that have nothing to show.
pub fn dark() -> Effect {
    solid(OFF)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::GREEN;
    use serde_json::json;

    #[test]
    fn breathe_starts_dark_and_peaks_halfway() {
        let mut effect = breathe(RED, 4.0, 0.0);
        assert_eq!(effect(0.0), OFF);
        assert_eq!(effect(2.0), RED);
    }

    #[test]
    fn sequence_holds_then_blends() {
        let steps = vec![Step::new(RED, 1.0, 0.0), Step::new(GREEN, 1.0, 1.0)];
        let mut effect = sequence(steps, true).unwrap();
        assert_eq!(effect(0.5), RED);
        assert_eq!(effect(1.5), Color::rgb(128, 128, 0));
        assert_eq!(effect(2.5), GREEN);
        assert_eq!(effect(3.5), RED);
    }

    #[test]
    fn custom_checks_its_steps() {
        assert!(custom(&[json!({"color": "#ff0000", "wait": 1})], 1.0).is_err());
        assert!(custom(&[json!({"hold": 1})], 1.0).is_err());
        assert!(custom(&[json!({"color": "red", "ease": "bouncy"})], 1.0).is_err());
        let mut effect =
            custom(&[json!({"color": "red", "hold": 1}), json!({"color": "blue", "hold": 1})], 2.0).unwrap();
        assert_eq!(effect(0.25), RED);
        assert_eq!(effect(0.75), BLUE);
    }

    #[test]
    fn along_blends_between_neighbours() {
        assert_eq!(along(&[RED, GREEN, BLUE], 0.0), RED);
        assert_eq!(along(&[RED, GREEN, BLUE], 0.5), GREEN);
        assert_eq!(along(&[RED, GREEN, BLUE], 0.75), Color::rgb(0, 128, 128));
        assert_eq!(along(&[RED, GREEN, BLUE], 7.0), BLUE);
        assert_eq!(along(&[RED], 0.3), RED);
    }

    #[test]
    fn aurora_stays_within_its_colours_and_depth() {
        let mut effect = aurora(&[GREEN, BLUE], 10.0, 0.5).unwrap();
        for frame in 0..2000 {
            let color = effect(frame as f64 * 0.05);
            assert_eq!(color.r, 0);
            assert!(color.g.max(color.b) >= 63, "{color:?}"); // half of the dimmer end of a blend
        }
        assert!(aurora(&[], 10.0, 0.5).is_err());
        assert!(aurora(&[RED], 0.0, 0.5).is_err());
    }

    #[test]
    fn fire_moves_between_embers_and_flame() {
        let (embers, flame) = (Color::rgb(255, 40, 0), Color::rgb(255, 180, 0));
        let mut effect = fire(embers, flame, 0.5, 1.0).unwrap();
        let greens: Vec<u8> = (0..400).map(|frame| effect(frame as f64 * 0.05).g).collect();
        assert!(greens.iter().all(|&g| (1..=180).contains(&g)));
        assert!(greens.iter().max().unwrap() - greens.iter().min().unwrap() > 60); // it does move
        assert!(fire(embers, flame, 0.5, 0.0).is_err());
    }

    #[test]
    fn lightning_strikes_out_of_a_dim_sky() {
        let (bolt, sky) = (Color::rgbw(0, 0, 0, 255), BLUE);
        let mut effect = lightning(bolt, sky, 0.2, 1.0, 2.0).unwrap();
        assert_eq!(effect(0.0), Color::rgb(0, 0, 51)); // the sky alone before the first strike
        let frames: Vec<Color> = (1..1200).map(|frame| effect(frame as f64 * 0.05)).collect();
        let strikes = frames.windows(2).filter(|pair| pair[0].w < 128 && pair[1].w >= 128).count();
        assert!((10..=60).contains(&strikes), "{strikes} strikes in a minute"); // about one every two seconds
        assert!(frames.iter().filter(|color| **color == Color::rgb(0, 0, 51)).count() > 200); // and dark in between
        let mut weak = lightning(bolt, sky, 0.0, 0.5, 2.0).unwrap();
        assert_eq!(weak(0.0), OFF);
        assert!((0..1200).map(|frame| weak(frame as f64 * 0.05).w).max().unwrap() <= 128);
        assert!(lightning(bolt, sky, 0.2, 1.0, 0.0).is_err());
        assert!(lightning(bolt, sky, 1.2, 1.0, 2.0).is_err());
    }

    #[test]
    fn sunrise_rises_once_and_stays() {
        let mut effect = sunrise(MORNING, 1.0).unwrap();
        assert_eq!(effect(0.0), OFF);
        let early = effect(6.0);
        assert!(early.r > 0 && early.r < 10 && early.w == 0, "{early:?}"); // a faint deep red
        let levels: Vec<u8> = (0..=60).map(|second| effect(second as f64).r).collect();
        assert!(levels.windows(2).all(|pair| pair[0] <= pair[1])); // never falls back
        assert_eq!(effect(60.0), MORNING);
        assert_eq!(effect(3600.0), MORNING);
        assert!(sunrise(MORNING, 0.0).is_err());
    }

    #[test]
    fn strobe_and_pulse() {
        let mut effect = strobe(RED, 1.0, 0.5, OFF);
        assert_eq!(effect(0.25), RED);
        assert_eq!(effect(0.75), OFF);
        let mut effect = pulse(RED, 1.0, 4.0, OFF);
        assert_eq!(effect(0.0), RED);
    }
}
