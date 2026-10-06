//! Effects that can be requested by name with plain parameters, as in `chsmartbulb.catalog`.
//!
//! [`describe`] gives front ends everything they need to build their controls, in
//! exactly the shape the Python service sends.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::audio::{self, AudioSource, DEFAULT_SENSITIVITY, MAX_DELAY};
use crate::color::{parse_color, Color, BLUE, GREEN, RED};
use crate::effects::{self, Effect, EASINGS, MAX_STEPS, WARM};
use crate::error::{invalid, Result};
use crate::screen::{self, ScreenSource};

/// The input an effect follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Needs {
    Audio,
    Screen,
}

impl Needs {
    pub fn as_str(self) -> &'static str {
        match self {
            Needs::Audio => "audio",
            Needs::Screen => "screen",
        }
    }
}

/// A parameter value once checked and filled in.
#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    Number(f64),
    Color(Option<Color>),
    Colors(Vec<Color>),
    Steps(Vec<Value>),
}

impl Param {
    fn to_json(&self) -> Value {
        match self {
            Param::Number(n) => json!(n),
            Param::Color(Some(color)) => json!(color.to_hex()),
            Param::Color(None) => Value::Null,
            Param::Colors(colors) => Value::Array(colors.iter().map(|c| json!(c.to_hex())).collect()),
            Param::Steps(steps) => Value::Array(steps.clone()),
        }
    }
}

pub type Resolved = BTreeMap<String, Param>;

/// What the effects that follow something are given to follow.
#[derive(Clone, Default)]
pub struct Sources {
    pub audio: Option<Arc<AudioSource>>,
    pub screen: Option<Arc<ScreenSource>>,
}

type Build = fn(&Resolved, &Sources) -> Result<Effect>;

pub struct EffectInfo {
    pub name: &'static str,
    pub summary: &'static str,
    pub needs: Option<Needs>,
    defaults: fn() -> Vec<(&'static str, Param)>,
    build: Build,
    ranges: &'static [(&'static str, (f64, f64, f64))],
}

/// Slider limits a front end offers per parameter: lowest, highest, step.
fn range(key: &str) -> (f64, f64, f64) {
    match key {
        "period" => (0.2, 60.0, 0.1),
        "floor" | "saturation" | "depth" | "white" | "balance" | "sensitivity" => (0.0, 1.0, 0.01),
        "decay" => (0.5, 20.0, 0.5),
        "hz" => (0.5, 10.0, 0.5),
        "duty" => (0.05, 0.95, 0.05),
        "hold" | "fade_in" => (0.0, 30.0, 0.5),
        "delay" => (0.0, MAX_DELAY, 0.01),
        "release" => (0.5, 10.0, 0.5),
        "width" => (1.0, 10.0, 0.5),
        "smoothing" => (0.0, 2.0, 0.05),
        "speed" => (0.1, 5.0, 0.1),
        "step" => (1.0, 180.0, 1.0),
        "slow" | "fast" => (40.0, 240.0, 1.0),
        _ => (0.0, 1.0, 0.01),
    }
}

fn n(params: &Resolved, key: &str) -> f64 {
    match params.get(key) {
        Some(Param::Number(value)) => *value,
        _ => unreachable!("{key} is a number parameter"),
    }
}

fn c(params: &Resolved, key: &str) -> Option<Color> {
    match params.get(key) {
        Some(Param::Color(value)) => *value,
        _ => unreachable!("{key} is a colour parameter"),
    }
}

fn colour(params: &Resolved, key: &str) -> Color {
    c(params, key).expect("a colour parameter with a default")
}

fn audio(sources: &Sources, name: &str) -> Result<Arc<AudioSource>> {
    sources.audio.clone().ok_or_else(|| invalid(format!("effect {name:?} was given no audio source to follow")))
}

fn custom_steps() -> Vec<Value> {
    ["#ff0000", "#ff8000", "#0040ff"]
        .iter()
        .map(|color| json!({"color": color, "hold": 1.0, "fade": 1.0, "ease": "ease-in-out"}))
        .collect()
}

pub static CATALOG: &[EffectInfo] = &[
    EffectInfo {
        name: "breathe",
        summary: "swell and fade",
        needs: None,
        defaults: || {
            vec![("color", Param::Color(Some(RED))), ("period", Param::Number(4.0)), ("floor", Param::Number(0.0))]
        },
        build: |p, _| Ok(effects::breathe(colour(p, "color"), n(p, "period"), n(p, "floor"))),
        ranges: &[],
    },
    EffectInfo {
        name: "hue",
        summary: "walk around the colour wheel",
        needs: None,
        defaults: || vec![("period", Param::Number(10.0)), ("saturation", Param::Number(1.0))],
        build: |p, _| Ok(effects::hue_cycle(n(p, "period"), n(p, "saturation"), 1.0)),
        ranges: &[],
    },
    EffectInfo {
        name: "pulse",
        summary: "flash, then decay",
        needs: None,
        defaults: || {
            vec![("color", Param::Color(Some(RED))), ("period", Param::Number(1.0)), ("decay", Param::Number(4.0))]
        },
        build: |p, _| Ok(effects::pulse(colour(p, "color"), n(p, "period"), n(p, "decay"), crate::color::OFF)),
        ranges: &[],
    },
    EffectInfo {
        name: "strobe",
        summary: "hard blinking",
        needs: None,
        defaults: || vec![("color", Param::Color(Some(RED))), ("hz", Param::Number(5.0)), ("duty", Param::Number(0.5))],
        build: |p, _| Ok(effects::strobe(colour(p, "color"), n(p, "hz"), n(p, "duty"), crate::color::OFF)),
        ranges: &[],
    },
    EffectInfo {
        name: "candle",
        summary: "uneven flicker",
        needs: None,
        defaults: || vec![("color", Param::Color(Some(WARM))), ("depth", Param::Number(0.6))],
        build: |p, _| Ok(effects::candle(colour(p, "color"), n(p, "depth"))),
        ranges: &[],
    },
    EffectInfo {
        name: "palette",
        summary: "drift through a list of colours",
        needs: None,
        defaults: || {
            vec![
                ("colors", Param::Colors(vec![RED, GREEN, BLUE])),
                ("hold", Param::Number(2.0)),
                ("fade_in", Param::Number(1.0)),
            ]
        },
        build: |p, _| match p.get("colors") {
            Some(Param::Colors(colors)) => effects::palette(colors, n(p, "hold"), n(p, "fade_in")),
            _ => unreachable!("colors is a colour list"),
        },
        ranges: &[],
    },
    EffectInfo {
        name: "police",
        summary: "alternate red and blue",
        needs: None,
        defaults: || vec![("period", Param::Number(1.0))],
        build: |p, _| effects::police(n(p, "period")),
        ranges: &[],
    },
    EffectInfo {
        name: "custom",
        summary: "your own colours, timings and fades",
        needs: None,
        defaults: || vec![("steps", Param::Steps(custom_steps())), ("speed", Param::Number(1.0))],
        build: |p, _| match p.get("steps") {
            Some(Param::Steps(steps)) => effects::custom(steps, n(p, "speed")),
            _ => unreachable!("steps is a step list"),
        },
        ranges: &[],
    },
    EffectInfo {
        name: "music",
        summary: "flash on the beat of the computer's audio",
        needs: Some(Needs::Audio),
        defaults: || {
            vec![
                ("color", Param::Color(None)),
                ("decay", Param::Number(5.0)),
                ("delay", Param::Number(0.0)),
                ("sensitivity", Param::Number(DEFAULT_SENSITIVITY)),
            ]
        },
        build: |p, s| {
            audio::music_pulse(audio(s, "music")?, c(p, "color"), n(p, "decay"), n(p, "delay"), n(p, "sensitivity"))
        },
        ranges: &[],
    },
    EffectInfo {
        name: "spectrum",
        summary: "bass, mids, treble as red, green, blue",
        needs: Some(Needs::Audio),
        defaults: || vec![("release", Param::Number(3.0)), ("delay", Param::Number(0.0))],
        build: |p, s| audio::music_spectrum(audio(s, "spectrum")?, n(p, "release"), n(p, "delay")),
        ranges: &[],
    },
    EffectInfo {
        name: "volume",
        summary: "one colour, as bright as the audio is loud",
        needs: Some(Needs::Audio),
        defaults: || {
            vec![
                ("color", Param::Color(Some(RED))),
                ("release", Param::Number(3.0)),
                ("floor", Param::Number(0.0)),
                ("delay", Param::Number(0.0)),
            ]
        },
        build: |p, s| {
            audio::music_volume(audio(s, "volume")?, colour(p, "color"), n(p, "release"), n(p, "floor"), n(p, "delay"))
        },
        ranges: &[],
    },
    EffectInfo {
        name: "stereo",
        summary: "blend two colours by where the sound sits between left and right",
        needs: Some(Needs::Audio),
        defaults: || {
            vec![
                ("left", Param::Color(Some(audio::STEREO_LEFT))),
                ("right", Param::Color(Some(audio::STEREO_RIGHT))),
                ("width", Param::Number(4.0)),
                ("release", Param::Number(3.0)),
                ("delay", Param::Number(0.0)),
            ]
        },
        build: |p, s| {
            let source = audio(s, "stereo")?;
            audio::music_stereo(
                source,
                colour(p, "left"),
                colour(p, "right"),
                n(p, "width"),
                n(p, "release"),
                n(p, "delay"),
            )
        },
        ranges: &[],
    },
    EffectInfo {
        name: "beathue",
        summary: "the colour turns on every beat, the brightness follows the bass",
        needs: Some(Needs::Audio),
        defaults: || {
            vec![
                ("step", Param::Number(47.0)),
                ("decay", Param::Number(5.0)),
                ("floor", Param::Number(0.1)),
                ("saturation", Param::Number(1.0)),
                ("delay", Param::Number(0.0)),
                ("sensitivity", Param::Number(DEFAULT_SENSITIVITY)),
            ]
        },
        build: |p, s| {
            audio::music_beathue(
                audio(s, "beathue")?,
                n(p, "step"),
                n(p, "decay"),
                n(p, "floor"),
                n(p, "saturation"),
                n(p, "delay"),
                n(p, "sensitivity"),
            )
        },
        ranges: &[],
    },
    EffectInfo {
        name: "tempo",
        summary: "warm for slow music, cool for fast, by the gaps between beats",
        needs: Some(Needs::Audio),
        defaults: || {
            vec![
                ("slow", Param::Number(80.0)),
                ("fast", Param::Number(160.0)),
                ("smoothing", Param::Number(2.0)),
                ("delay", Param::Number(0.0)),
                ("sensitivity", Param::Number(DEFAULT_SENSITIVITY)),
            ]
        },
        build: |p, s| {
            audio::music_tempo(
                audio(s, "tempo")?,
                n(p, "slow"),
                n(p, "fast"),
                n(p, "smoothing"),
                n(p, "delay"),
                n(p, "sensitivity"),
            )
        },
        ranges: &[],
    },
    EffectInfo {
        name: "centroid",
        summary: "blend two colours by whether the sound is bass-heavy or bright",
        needs: Some(Needs::Audio),
        defaults: || {
            vec![
                ("low", Param::Color(Some(RED))),
                ("high", Param::Color(Some(BLUE))),
                ("width", Param::Number(2.0)),
                ("release", Param::Number(3.0)),
                ("delay", Param::Number(0.0)),
            ]
        },
        build: |p, s| {
            audio::music_centroid(
                audio(s, "centroid")?,
                colour(p, "low"),
                colour(p, "high"),
                n(p, "width"),
                n(p, "release"),
                n(p, "delay"),
            )
        },
        ranges: &[],
    },
    EffectInfo {
        name: "screen",
        summary: "follow the colour of the screen",
        needs: Some(Needs::Screen),
        defaults: || {
            vec![
                ("smoothing", Param::Number(0.2)),
                ("saturation", Param::Number(1.5)),
                ("white", Param::Number(1.0)),
                ("balance", Param::Number(0.0)),
            ]
        },
        build: |p, s| {
            let source =
                s.screen.clone().ok_or_else(|| invalid("effect \"screen\" was given no screen source to follow"))?;
            screen::screen_follow(source, n(p, "smoothing"), n(p, "saturation"), n(p, "white"), n(p, "balance"))
        },
        ranges: &[("saturation", (0.0, 3.0, 0.1))],
    },
];

pub fn lookup(name: &str) -> Option<&'static EffectInfo> {
    CATALOG.iter().find(|info| info.name == name)
}

fn names() -> String {
    CATALOG.iter().map(|info| info.name).collect::<Vec<_>>().join(", ")
}

fn as_color(value: &Value) -> Result<Color> {
    match value {
        Value::String(text) => parse_color(text),
        other => parse_color(&other.to_string()),
    }
}

fn coerce(key: &str, default: &Param, value: &Value) -> Result<Param> {
    match default {
        Param::Color(fallback) => {
            if value.is_null() && fallback.is_none() {
                Ok(Param::Color(None))
            } else {
                Ok(Param::Color(Some(as_color(value)?)))
            }
        }
        Param::Colors(_) => {
            let colors = match value {
                Value::String(text) => text.split(',').map(parse_color).collect::<Result<Vec<_>>>()?,
                Value::Array(items) => items.iter().map(as_color).collect::<Result<Vec<_>>>()?,
                other => return Err(invalid(format!("colors must be a list of colours, got {other}"))),
            };
            if colors.is_empty() {
                return Err(invalid("colors must not be empty"));
            }
            Ok(Param::Colors(colors))
        }
        Param::Steps(_) => {
            let parsed;
            let value = match value {
                Value::String(text) => {
                    // from the command line
                    parsed = serde_json::from_str::<Value>(text).unwrap_or(Value::Null);
                    &parsed
                }
                other => other,
            };
            match value {
                Value::Array(steps) => Ok(Param::Steps(steps.clone())), // the effect checks each step
                _ => Err(invalid("steps must be a list of steps, as JSON when given as text")),
            }
        }
        Param::Number(_) => {
            let number = match value {
                Value::Number(number) => number.as_f64(),
                Value::String(text) => text.trim().parse::<f64>().ok(),
                _ => None,
            };
            number.map(Param::Number).ok_or_else(|| invalid(format!("{key} must be a number, got {value}")))
        }
    }
}

/// Validate `params` for effect `name` and fill in the defaults.
pub fn resolve(name: &str, params: Option<&Map<String, Value>>) -> Result<Resolved> {
    let info = lookup(name).ok_or_else(|| invalid(format!("unknown effect {name:?}; available: {}", names())))?;
    let defaults = (info.defaults)();
    let mut resolved: Resolved = defaults.iter().map(|(key, value)| (key.to_string(), value.clone())).collect();
    for (key, value) in params.into_iter().flatten() {
        let Some((_, default)) = defaults.iter().find(|(known, _)| known == key) else {
            let takes = defaults.iter().map(|(key, _)| *key).collect::<Vec<_>>().join(", ");
            return Err(invalid(format!("effect {name:?} has no parameter {key:?}; it takes: {takes}")));
        };
        resolved.insert(key.clone(), coerce(key, default, value)?);
    }
    Ok(resolved)
}

/// Build the named effect. Those that follow sound or the screen need that source.
pub fn create(name: &str, params: Option<&Map<String, Value>>, sources: &Sources) -> Result<Effect> {
    let resolved = resolve(name, params)?;
    let info = lookup(name).expect("resolved above");
    (info.build)(&resolved, sources)
}

fn schema(info: &EffectInfo, key: &str, default: &Param) -> Value {
    match default {
        Param::Colors(_) => json!({"type": "colors"}),
        Param::Steps(_) => json!({"type": "steps", "most": MAX_STEPS, "easings": EASINGS}),
        Param::Color(fallback) => json!({"type": "color", "optional": fallback.is_none()}),
        Param::Number(_) => {
            let (low, high, step) = info
                .ranges
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, range)| *range)
                .unwrap_or_else(|| range(key));
            json!({"type": "number", "min": low, "max": high, "step": step})
        }
    }
}

/// The catalog as plain data, for listings and for front ends that build their controls from it.
pub fn describe() -> Value {
    Value::Array(
        CATALOG
            .iter()
            .map(|info| {
                let defaults = (info.defaults)();
                let mut params = Map::new();
                let mut schemas = Map::new();
                for (key, default) in &defaults {
                    params.insert(key.to_string(), default.to_json());
                    schemas.insert(key.to_string(), schema(info, key, default));
                }
                json!({
                    "name": info.name,
                    "summary": info.summary,
                    "params": params,
                    "schema": schemas,
                    "needs": info.needs.map(Needs::as_str),
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn resolve_fills_defaults_and_coerces() {
        let resolved = resolve("breathe", Some(&params(json!({"color": "blue", "period": "2"})))).unwrap();
        assert_eq!(resolved["color"], Param::Color(Some(BLUE)));
        assert_eq!(resolved["period"], Param::Number(2.0));
        assert_eq!(resolved["floor"], Param::Number(0.0));
        let resolved = resolve("palette", Some(&params(json!({"colors": "red,#00ff00"})))).unwrap();
        assert_eq!(resolved["colors"], Param::Colors(vec![RED, GREEN]));
    }

    #[test]
    fn resolve_rejects_what_does_not_fit() {
        assert!(resolve("sparkle", None).is_err());
        assert!(resolve("breathe", Some(&params(json!({"speed": 1})))).is_err());
        assert!(resolve("breathe", Some(&params(json!({"period": true})))).is_err());
        assert!(resolve("palette", Some(&params(json!({"colors": []})))).is_err());
        assert!(resolve("custom", Some(&params(json!({"steps": "not json"})))).is_err());
    }

    #[test]
    fn music_needs_a_source() {
        assert!(create("music", None, &Sources::default()).is_err());
        let sources = Sources { audio: Some(AudioSource::new(audio::monotonic())), screen: None };
        assert!(create("music", None, &sources).is_ok());
        assert!(create("hue", None, &Sources::default()).is_ok());
    }

    #[test]
    fn describe_matches_the_python_shape() {
        let described = describe();
        let all = described.as_array().unwrap();
        assert_eq!(all.len(), 16);
        let music = all.iter().find(|e| e["name"] == "music").unwrap();
        assert_eq!(music["needs"], "audio");
        assert_eq!(music["params"]["color"], Value::Null);
        assert_eq!(music["schema"]["color"], json!({"type": "color", "optional": true}));
        assert_eq!(music["schema"]["delay"], json!({"type": "number", "min": 0.0, "max": 2.0, "step": 0.01}));
        let beathue = all.iter().find(|e| e["name"] == "beathue").unwrap();
        assert_eq!(beathue["needs"], "audio");
        assert_eq!(beathue["schema"]["step"], json!({"type": "number", "min": 1.0, "max": 180.0, "step": 1.0}));
        let screen = all.iter().find(|e| e["name"] == "screen").unwrap();
        assert_eq!(screen["schema"]["saturation"]["max"], 3.0);
        let custom = all.iter().find(|e| e["name"] == "custom").unwrap();
        assert_eq!(custom["schema"]["steps"]["most"], 16);
        assert_eq!(custom["params"]["steps"][0]["ease"], "ease-in-out");
        let palette = all.iter().find(|e| e["name"] == "palette").unwrap();
        assert_eq!(palette["params"]["colors"], json!(["#ff0000", "#00ff00", "#0000ff"]));
        assert_eq!(all.iter().find(|e| e["name"] == "candle").unwrap()["params"]["color"], "#ff6e14");
    }
}
