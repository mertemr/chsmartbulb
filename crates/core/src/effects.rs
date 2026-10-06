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
    fn strobe_and_pulse() {
        let mut effect = strobe(RED, 1.0, 0.5, OFF);
        assert_eq!(effect(0.25), RED);
        assert_eq!(effect(0.75), OFF);
        let mut effect = pulse(RED, 1.0, 4.0, OFF);
        assert_eq!(effect(0.0), RED);
    }
}
