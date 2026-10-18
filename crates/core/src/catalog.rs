//! Effects that can be requested by name with plain parameters.
//!
//! [`describe`] gives front ends everything they need to build their controls.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::audio::{self, AudioSource, Follow, DEFAULT_SENSITIVITY, FOLLOWS, MAX_DELAY, PAN_SMOOTHING};
use crate::color::{parse_color, Color, BLUE, GREEN, RED, WHITE};
use crate::effects::{self, Effect, EASINGS, MAX_STEPS, MORNING, WARM};
use crate::error::{invalid, Result};
use crate::screen::{self, ScreenSource, Sway, MAX_SHIFT};

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
    /// One of a fixed set of names.
    Choice {
        options: &'static [&'static str],
        value: String,
    },
}

impl Param {
    fn choice(options: &'static [&'static str], value: &str) -> Param {
        Param::Choice { options, value: value.to_string() }
    }
}

impl Param {
    fn to_json(&self) -> Value {
        match self {
            Param::Number(n) => json!(n),
            Param::Color(Some(color)) => json!(color.to_hex()),
            Param::Color(None) => Value::Null,
            Param::Colors(colors) => Value::Array(colors.iter().map(|c| json!(c.to_hex())).collect()),
            Param::Steps(steps) => Value::Array(steps.clone()),
            Param::Choice { value, .. } => json!(value),
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
    /// The input an effect follows, and the group front ends list it under.
    pub needs: Option<Needs>,
    /// A second input, for an effect that follows both.
    pub also: Option<Needs>,
    defaults: fn() -> Vec<(&'static str, Param)>,
    build: Build,
    ranges: &'static [(&'static str, (f64, f64, f64))],
}

impl EffectInfo {
    /// Whether the effect follows `kind`.
    pub fn follows(&self, kind: Needs) -> bool {
        self.needs == Some(kind) || self.also == Some(kind)
    }
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
        "flash" => (0.1, 2.0, 0.05),
        "build" => (0.5, 10.0, 0.5),
        "glow" | "power" => (0.0, 1.0, 0.01),
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

fn screen(sources: &Sources, name: &str) -> Result<Arc<ScreenSource>> {
    sources.screen.clone().ok_or_else(|| invalid(format!("effect {name:?} was given no screen source to follow")))
}

fn choice<'a>(params: &'a Resolved, key: &str) -> &'a str {
    match params.get(key) {
        Some(Param::Choice { value, .. }) => value,
        _ => unreachable!("{key} is a choice parameter"),
    }
}

fn colours<'a>(params: &'a Resolved, key: &str) -> &'a [Color] {
    match params.get(key) {
        Some(Param::Colors(colors)) => colors,
        _ => unreachable!("{key} is a colour list"),
    }
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
        also: None,
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
        also: None,
        defaults: || vec![("period", Param::Number(10.0)), ("saturation", Param::Number(1.0))],
        build: |p, _| Ok(effects::hue_cycle(n(p, "period"), n(p, "saturation"), 1.0)),
        ranges: &[],
    },
    EffectInfo {
        name: "pulse",
        summary: "flash, then decay",
        needs: None,
        also: None,
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
        also: None,
        defaults: || vec![("color", Param::Color(Some(RED))), ("hz", Param::Number(5.0)), ("duty", Param::Number(0.5))],
        build: |p, _| Ok(effects::strobe(colour(p, "color"), n(p, "hz"), n(p, "duty"), crate::color::OFF)),
        ranges: &[],
    },
    EffectInfo {
        name: "candle",
        summary: "uneven flicker",
        needs: None,
        also: None,
        defaults: || vec![("color", Param::Color(Some(WARM))), ("depth", Param::Number(0.6))],
        build: |p, _| Ok(effects::candle(colour(p, "color"), n(p, "depth"))),
        ranges: &[],
    },
    EffectInfo {
        name: "palette",
        summary: "drift through a list of colours",
        needs: None,
        also: None,
        defaults: || {
            vec![
                ("colors", Param::Colors(vec![RED, GREEN, BLUE])),
                ("hold", Param::Number(2.0)),
                ("fade_in", Param::Number(1.0)),
            ]
        },
        build: |p, _| effects::palette(colours(p, "colors"), n(p, "hold"), n(p, "fade_in")),
        ranges: &[],
    },
    EffectInfo {
        name: "police",
        summary: "alternate red and blue",
        needs: None,
        also: None,
        defaults: || vec![("period", Param::Number(1.0))],
        build: |p, _| effects::police(n(p, "period")),
        ranges: &[],
    },
    EffectInfo {
        name: "aurora",
        summary: "slow curtains of light wandering through a few colours",
        needs: None,
        also: None,
        defaults: || {
            vec![
                (
                    "colors",
                    Param::Colors(vec![Color::rgb(0, 255, 80), Color::rgb(0, 200, 180), Color::rgb(128, 0, 255)]),
                ),
                ("period", Param::Number(20.0)),
                ("depth", Param::Number(0.5)),
            ]
        },
        build: |p, _| effects::aurora(colours(p, "colors"), n(p, "period"), n(p, "depth")),
        ranges: &[("period", (2.0, 120.0, 1.0))],
    },
    EffectInfo {
        name: "fire",
        summary: "embers that flare up into flames",
        needs: None,
        also: None,
        defaults: || {
            vec![
                ("color", Param::Color(Some(Color::rgb(255, 40, 0)))),
                ("flame", Param::Color(Some(Color::rgb(255, 180, 0)))),
                ("depth", Param::Number(0.6)),
                ("speed", Param::Number(1.0)),
            ]
        },
        build: |p, _| effects::fire(colour(p, "color"), colour(p, "flame"), n(p, "depth"), n(p, "speed")),
        ranges: &[],
    },
    EffectInfo {
        name: "lightning",
        summary: "a dim sky, and strikes that flicker and die away",
        needs: None,
        also: None,
        defaults: || {
            vec![
                ("color", Param::Color(Some(Color::rgbw(140, 170, 255, 255)))),
                ("sky", Param::Color(Some(Color::rgb(30, 0, 255)))),
                ("glow", Param::Number(0.1)),
                ("power", Param::Number(1.0)),
                ("gap", Param::Number(6.0)),
            ]
        },
        build: |p, _| {
            effects::lightning(colour(p, "color"), colour(p, "sky"), n(p, "glow"), n(p, "power"), n(p, "gap"))
        },
        ranges: &[("gap", (1.0, 30.0, 0.5))],
    },
    EffectInfo {
        name: "sunrise",
        summary: "from dark through red and orange to a warm morning light, then it stays",
        needs: None,
        also: None,
        defaults: || vec![("color", Param::Color(Some(MORNING))), ("minutes", Param::Number(20.0))],
        build: |p, _| effects::sunrise(colour(p, "color"), n(p, "minutes")),
        ranges: &[("minutes", (1.0, 60.0, 1.0))],
    },
    EffectInfo {
        name: "custom",
        summary: "your own colours, timings and fades",
        needs: None,
        also: None,
        defaults: || vec![("steps", Param::Steps(custom_steps())), ("speed", Param::Number(1.0))],
        build: |p, _| match p.get("steps") {
            Some(Param::Steps(steps)) => effects::custom(steps, n(p, "speed")),
            _ => unreachable!("steps is a step list"),
        },
        ranges: &[],
    },
    EffectInfo {
        name: "music",
        summary: "flash on the beat, a new colour on each one unless you pick one",
        needs: Some(Needs::Audio),
        also: None,
        defaults: || {
            vec![
                ("color", Param::Color(None)),
                ("decay", Param::Number(5.0)),
                ("floor", Param::Number(0.0)),
                ("step", Param::Number(47.0)),
                ("saturation", Param::Number(1.0)),
                ("delay", Param::Number(0.0)),
                ("sensitivity", Param::Number(DEFAULT_SENSITIVITY)),
            ]
        },
        build: |p, s| {
            let beat = audio::Beat {
                step: n(p, "step"),
                decay: n(p, "decay"),
                floor: n(p, "floor"),
                saturation: n(p, "saturation"),
            };
            audio::music_beat(audio(s, "music")?, c(p, "color"), beat, n(p, "delay"), n(p, "sensitivity"))
        },
        ranges: &[],
    },
    EffectInfo {
        name: "spectrum",
        summary: "bass, mids, treble as red, green, blue",
        needs: Some(Needs::Audio),
        also: None,
        defaults: || vec![("release", Param::Number(3.0)), ("delay", Param::Number(0.0))],
        build: |p, s| audio::music_spectrum(audio(s, "spectrum")?, n(p, "release"), n(p, "delay")),
        ranges: &[],
    },
    EffectInfo {
        name: "volume",
        summary: "one colour, as bright as the audio is loud",
        needs: Some(Needs::Audio),
        also: None,
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
        name: "blend",
        summary: "two colours, blended by where the sound sits, how bright it is or how fast it runs",
        needs: Some(Needs::Audio),
        also: None,
        defaults: || {
            vec![
                ("follow", Param::choice(&FOLLOWS, "balance")),
                ("low", Param::Color(Some(audio::STEREO_LEFT))),
                ("high", Param::Color(Some(audio::STEREO_RIGHT))),
                ("width", Param::Number(4.0)),
                ("slow", Param::Number(80.0)),
                ("fast", Param::Number(160.0)),
                ("smoothing", Param::Number(PAN_SMOOTHING)),
                ("release", Param::Number(3.0)),
                ("delay", Param::Number(0.0)),
                ("sensitivity", Param::Number(DEFAULT_SENSITIVITY)),
            ]
        },
        build: |p, s| {
            let name = choice(p, "follow");
            let follow = Follow::from_name(name)
                .ok_or_else(|| invalid(format!("unknown follow {name:?}; choose from: {}", FOLLOWS.join(", "))))?;
            let blend = audio::Blend {
                follow,
                low: colour(p, "low"),
                high: colour(p, "high"),
                width: n(p, "width"),
                slow: n(p, "slow"),
                fast: n(p, "fast"),
                smoothing: n(p, "smoothing"),
                release: n(p, "release"),
            };
            audio::music_blend(audio(s, "blend")?, blend, n(p, "delay"), n(p, "sensitivity"))
        },
        ranges: &[("smoothing", (0.0, 5.0, 0.05))],
    },
    EffectInfo {
        name: "drop",
        summary: "opens up as the music builds, flashes when it drops back in",
        needs: Some(Needs::Audio),
        also: None,
        defaults: || {
            vec![
                ("color", Param::Color(Some(WHITE))),
                ("flash", Param::Number(0.4)),
                ("build", Param::Number(3.0)),
                ("delay", Param::Number(0.0)),
                ("sensitivity", Param::Number(DEFAULT_SENSITIVITY)),
            ]
        },
        build: |p, s| {
            audio::music_drop(
                audio(s, "drop")?,
                colour(p, "color"),
                n(p, "flash"),
                n(p, "build"),
                n(p, "delay"),
                n(p, "sensitivity"),
            )
        },
        ranges: &[],
    },
    EffectInfo {
        name: "ambient",
        summary: "a calm colour that breathes, an accent on the beats; never dark",
        needs: Some(Needs::Audio),
        also: None,
        defaults: || {
            vec![
                ("base", Param::Color(Some(WARM))),
                ("accent", Param::Color(Some(WHITE))),
                ("period", Param::Number(6.0)),
                ("decay", Param::Number(5.0)),
                ("delay", Param::Number(0.0)),
                ("sensitivity", Param::Number(DEFAULT_SENSITIVITY)),
            ]
        },
        build: |p, s| {
            audio::music_ambient(
                audio(s, "ambient")?,
                colour(p, "base"),
                colour(p, "accent"),
                n(p, "period"),
                n(p, "decay"),
                n(p, "delay"),
                n(p, "sensitivity"),
            )
        },
        ranges: &[],
    },
    EffectInfo {
        name: "bands",
        summary: "the bass in one colour, the treble in another",
        needs: Some(Needs::Audio),
        also: None,
        defaults: || {
            vec![
                ("low", Param::Color(Some(audio::BANDS_LOW))),
                ("high", Param::Color(Some(audio::BANDS_HIGH))),
                ("release", Param::Number(4.0)),
                ("delay", Param::Number(0.0)),
            ]
        },
        build: |p, s| {
            audio::music_bands(audio(s, "bands")?, colour(p, "low"), colour(p, "high"), n(p, "release"), n(p, "delay"))
        },
        ranges: &[],
    },
    EffectInfo {
        name: "energy",
        summary: "the colour by how intense the music has been, calm to wild",
        needs: Some(Needs::Audio),
        also: None,
        defaults: || {
            vec![
                ("colors", Param::Colors(audio::ENERGY_COLORS.to_vec())),
                ("smoothing", Param::Number(4.0)),
                ("floor", Param::Number(0.1)),
                ("delay", Param::Number(0.0)),
            ]
        },
        build: |p, s| {
            audio::music_energy(
                audio(s, "energy")?,
                colours(p, "colors"),
                n(p, "smoothing"),
                n(p, "floor"),
                n(p, "delay"),
            )
        },
        ranges: &[("smoothing", (0.5, 20.0, 0.5))],
    },
    EffectInfo {
        name: "chill",
        summary: "a wandering colour that swells and sinks with the music, without flashing",
        needs: Some(Needs::Audio),
        also: None,
        defaults: || {
            vec![
                ("period", Param::Number(30.0)),
                ("saturation", Param::Number(1.0)),
                ("floor", Param::Number(0.15)),
                ("smoothing", Param::Number(1.5)),
                ("delay", Param::Number(0.0)),
            ]
        },
        build: |p, s| {
            audio::music_chill(
                audio(s, "chill")?,
                n(p, "period"),
                n(p, "saturation"),
                n(p, "floor"),
                n(p, "smoothing"),
                n(p, "delay"),
            )
        },
        ranges: &[("period", (2.0, 120.0, 1.0)), ("smoothing", (0.0, 5.0, 0.1))],
    },
    EffectInfo {
        name: "screen",
        summary: "follow the colour of the screen",
        needs: Some(Needs::Screen),
        also: None,
        defaults: || {
            vec![
                ("smoothing", Param::Number(0.2)),
                ("saturation", Param::Number(1.5)),
                ("white", Param::Number(1.0)),
                ("balance", Param::Number(0.0)),
            ]
        },
        build: |p, s| {
            screen::screen_follow(
                screen(s, "screen")?,
                n(p, "smoothing"),
                n(p, "saturation"),
                n(p, "white"),
                n(p, "balance"),
            )
        },
        ranges: &[("saturation", (0.0, 3.0, 0.1))],
    },
    EffectInfo {
        name: "screensound",
        summary: "the colour of the screen, as bright as the sound is loud",
        needs: Some(Needs::Screen),
        also: Some(Needs::Audio),
        defaults: || {
            vec![
                ("smoothing", Param::Number(0.2)),
                ("saturation", Param::Number(1.5)),
                ("white", Param::Number(1.0)),
                ("balance", Param::Number(0.0)),
                ("floor", Param::Number(0.35)),
                ("release", Param::Number(2.0)),
                ("shift", Param::Number(12.0)),
                ("delay", Param::Number(0.0)),
            ]
        },
        build: |p, s| {
            let source = screen(s, "screensound")?;
            let sway =
                Sway { floor: n(p, "floor"), release: n(p, "release"), shift: n(p, "shift"), delay: n(p, "delay") };
            screen::screen_sound(
                source,
                audio(s, "screensound")?,
                n(p, "smoothing"),
                n(p, "saturation"),
                n(p, "white"),
                n(p, "balance"),
                sway,
            )
        },
        ranges: &[("saturation", (0.0, 3.0, 0.1)), ("shift", (0.0, MAX_SHIFT, 1.0))],
    },
    EffectInfo {
        name: "screencut",
        summary: "the colour of the screen, with a flash when the scene cuts",
        needs: Some(Needs::Screen),
        also: None,
        defaults: || {
            vec![
                ("smoothing", Param::Number(0.2)),
                ("saturation", Param::Number(1.5)),
                ("white", Param::Number(1.0)),
                ("balance", Param::Number(0.0)),
                ("flash", Param::Number(0.6)),
                ("decay", Param::Number(6.0)),
            ]
        },
        build: |p, s| {
            screen::screen_cut(
                screen(s, "screencut")?,
                n(p, "smoothing"),
                n(p, "saturation"),
                n(p, "white"),
                n(p, "balance"),
                n(p, "flash"),
                n(p, "decay"),
            )
        },
        ranges: &[("saturation", (0.0, 3.0, 0.1)), ("flash", (0.0, 1.0, 0.05))],
    },
    EffectInfo {
        name: "screenhue",
        summary: "only the hue of the screen, fully lit even when the picture is dark",
        needs: Some(Needs::Screen),
        also: None,
        defaults: || {
            vec![("smoothing", Param::Number(1.0)), ("saturation", Param::Number(1.0)), ("balance", Param::Number(0.0))]
        },
        build: |p, s| {
            screen::screen_hue(screen(s, "screenhue")?, n(p, "smoothing"), n(p, "saturation"), n(p, "balance"))
        },
        ranges: &[],
    },
];

/// A name that was its own effect once and is now a setting of another.
///
/// It still runs, so a saved effect and an old script keep working, but [`describe`]
/// lists only what it became.
struct Alias {
    old: &'static str,
    now: &'static str,
    /// What the old name stands for, under anything the caller gives.
    preset: fn() -> Vec<(&'static str, Param)>,
}

static ALIASES: &[Alias] = &[
    Alias { old: "beathue", now: "music", preset: || vec![("floor", Param::Number(0.1))] },
    Alias {
        old: "stereo",
        now: "blend",
        preset: || {
            vec![
                ("follow", Param::choice(&FOLLOWS, "balance")),
                ("low", Param::Color(Some(audio::STEREO_LEFT))),
                ("high", Param::Color(Some(audio::STEREO_RIGHT))),
                ("width", Param::Number(4.0)),
            ]
        },
    },
    Alias {
        old: "centroid",
        now: "blend",
        preset: || {
            vec![
                ("follow", Param::choice(&FOLLOWS, "brightness")),
                ("low", Param::Color(Some(RED))),
                ("high", Param::Color(Some(BLUE))),
                ("width", Param::Number(2.0)),
            ]
        },
    },
    Alias {
        old: "tempo",
        now: "blend",
        preset: || {
            vec![
                ("follow", Param::choice(&FOLLOWS, "tempo")),
                ("low", Param::Color(Some(audio::TEMPO_SLOW))),
                ("high", Param::Color(Some(audio::TEMPO_FAST))),
                ("smoothing", Param::Number(2.0)),
            ]
        },
    },
];

pub fn lookup(name: &str) -> Option<&'static EffectInfo> {
    let name = ALIASES.iter().find(|alias| alias.old == name).map_or(name, |alias| alias.now);
    CATALOG.iter().find(|info| info.name == name)
}

/// The catalog name for `name` and the parameters it carries, under those given.
///
/// A service stores these, so what it saved goes on meaning the same once the old
/// name is only an alias.
pub fn canonical(name: &str, params: &Map<String, Value>) -> (String, Map<String, Value>) {
    let Some(alias) = ALIASES.iter().find(|alias| alias.old == name) else {
        return (name.to_string(), params.clone());
    };
    let mut merged: Map<String, Value> =
        (alias.preset)().iter().map(|(key, value)| (key.to_string(), value.to_json())).collect();
    merged.extend(params.iter().map(|(key, value)| (key.clone(), value.clone())));
    (alias.now.to_string(), merged)
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
        Param::Choice { options, .. } => {
            let Value::String(text) = value else {
                return Err(invalid(format!("{key} must be one of: {}", options.join(", "))));
            };
            if !options.contains(&text.as_str()) {
                return Err(invalid(format!("unknown {key} {text:?}; choose from: {}", options.join(", "))));
            }
            Ok(Param::choice(options, text))
        }
        Param::Number(_) => {
            let number = match value {
                Value::Number(number) => number.as_f64(),
                Value::String(text) => text.trim().parse::<f64>().ok(),
                _ => None,
            };
            match number {
                Some(number) if !number.is_finite() => {
                    Err(invalid(format!("{key} must be a finite number, got {value}")))
                }
                Some(number) => Ok(Param::Number(number)),
                None => Err(invalid(format!("{key} must be a number, got {value}"))),
            }
        }
    }
}

/// Validate `params` for effect `name` and fill in the defaults.
pub fn resolve(name: &str, params: Option<&Map<String, Value>>) -> Result<Resolved> {
    let info = lookup(name).ok_or_else(|| invalid(format!("unknown effect {name:?}; available: {}", names())))?;
    let defaults = (info.defaults)();
    let mut resolved: Resolved = defaults.iter().map(|(key, value)| (key.to_string(), value.clone())).collect();
    if let Some(alias) = ALIASES.iter().find(|alias| alias.old == name) {
        resolved.extend((alias.preset)().into_iter().map(|(key, value)| (key.to_string(), value)));
    }
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

/// Parameters a front end can leave out of its controls: they only apply for some values
/// of a choice. The effect, the parameter, the choice that decides, and the values it applies for.
static ONLY_WHEN: &[(&str, &str, &str, &[&str])] = &[
    ("blend", "width", "follow", &["balance", "brightness"]),
    ("blend", "slow", "follow", &["tempo"]),
    ("blend", "fast", "follow", &["tempo"]),
    ("blend", "sensitivity", "follow", &["tempo"]),
];

fn schema(info: &EffectInfo, key: &str, default: &Param) -> Value {
    let mut described = match default {
        Param::Colors(_) => json!({"type": "colors"}),
        Param::Steps(_) => json!({"type": "steps", "most": MAX_STEPS, "easings": EASINGS}),
        Param::Color(fallback) => json!({"type": "color", "optional": fallback.is_none()}),
        Param::Choice { options, .. } => json!({"type": "choice", "options": options}),
        Param::Number(_) => {
            let (low, high, step) = info
                .ranges
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, range)| *range)
                .unwrap_or_else(|| range(key));
            json!({"type": "number", "min": low, "max": high, "step": step})
        }
    };
    let only = ONLY_WHEN.iter().find(|(effect, param, ..)| *effect == info.name && *param == key);
    if let (Some((.., decides, values)), Some(object)) = (only, described.as_object_mut()) {
        object.insert("when".to_string(), json!({ *decides: values }));
    }
    described
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
                    "also": info.also.map(Needs::as_str),
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
    fn an_old_name_still_runs_what_it_became() {
        let sources = Sources { audio: Some(AudioSource::new(audio::monotonic())), screen: None };
        for (old, now) in [("beathue", "music"), ("stereo", "blend"), ("centroid", "blend"), ("tempo", "blend")] {
            assert_eq!(lookup(old).unwrap().name, now, "{old}");
            assert!(create(old, None, &sources).is_ok(), "{old}");
        }
        // the old name brings what it stood for, and what the caller gives still wins
        let resolved = resolve("beathue", Some(&params(json!({"step": 90})))).unwrap();
        assert_eq!((&resolved["step"], &resolved["floor"]), (&Param::Number(90.0), &Param::Number(0.1)));
        let resolved = resolve("tempo", Some(&params(json!({"smoothing": 0.85})))).unwrap();
        assert_eq!(resolved["follow"], Param::choice(&FOLLOWS, "tempo"));
        assert_eq!(resolved["smoothing"], Param::Number(0.85));
        assert_eq!(resolved["low"], Param::Color(Some(audio::TEMPO_SLOW)));
        assert!(resolve("blend", Some(&params(json!({"follow": "mood"})))).is_err());
    }

    #[test]
    fn canonical_hands_on_the_name_and_what_it_stood_for() {
        let given = params(json!({"smoothing": 0.85}));
        let (name, carried) = canonical("tempo", &given);
        assert_eq!(name, "blend");
        assert_eq!(carried["follow"], "tempo");
        assert_eq!(carried["smoothing"], 0.85); // the caller's, not the preset's 2.0
        assert_eq!(carried["high"], audio::TEMPO_FAST.to_hex());
        // a name the catalog lists is handed back untouched
        assert_eq!(canonical("blend", &given), ("blend".to_string(), given));
    }

    #[test]
    fn numbers_must_be_finite() {
        for bad in ["inf", "nan", "-inf"] {
            assert!(resolve("beathue", Some(&params(json!({"decay": bad})))).is_err());
        }
        let resolved = resolve("beathue", Some(&params(json!({"decay": "2.5"})))).unwrap();
        assert_eq!(resolved["decay"], Param::Number(2.5));
    }

    #[test]
    fn music_needs_a_source() {
        assert!(create("music", None, &Sources::default()).is_err());
        let sources = Sources { audio: Some(AudioSource::new(audio::monotonic())), screen: None };
        assert!(create("music", None, &sources).is_ok());
        assert!(create("hue", None, &Sources::default()).is_ok());
        assert!(create("screensound", None, &sources).is_err()); // it follows the screen as well
        let sources = Sources { screen: Some(ScreenSource::new()), ..sources };
        assert!(create("screensound", None, &sources).is_ok());
    }

    #[test]
    fn every_effect_builds_from_its_defaults_and_plays() {
        let sources = Sources { audio: Some(AudioSource::new(audio::monotonic())), screen: Some(ScreenSource::new()) };
        for info in CATALOG {
            let mut effect = create(info.name, None, &sources).unwrap_or_else(|error| panic!("{}: {error}", info.name));
            for frame in 0..100 {
                effect(frame as f64 * 0.05);
            }
        }
    }

    #[test]
    fn describe_is_plain_data_covering_every_effect() {
        let described = describe();
        let all = described.as_array().unwrap();
        assert_eq!(all.len(), 25);
        let names: Vec<&str> = all.iter().map(|e| e["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "breathe",
                "hue",
                "pulse",
                "strobe",
                "candle",
                "palette",
                "police",
                "aurora",
                "fire",
                "lightning",
                "sunrise",
                "custom",
                "music",
                "spectrum",
                "volume",
                "blend",
                "drop",
                "ambient",
                "bands",
                "energy",
                "chill",
                "screen",
                "screensound",
                "screencut",
                "screenhue"
            ]
        );
        let of = |name: &str| all.iter().find(|e| e["name"] == name).unwrap();
        assert_eq!(of("lightning")["params"]["color"], "#8caaffff");
        assert_eq!(of("lightning")["schema"]["gap"], json!({"type": "number", "min": 1.0, "max": 30.0, "step": 0.5}));
        assert_eq!(of("sunrise")["schema"]["minutes"]["max"], 60.0);
        assert_eq!(of("energy")["schema"]["colors"], json!({"type": "colors"}));
        assert_eq!((&of("energy")["needs"], &of("screencut")["needs"]), (&json!("audio"), &json!("screen")));
        assert_eq!(of("screencut")["schema"]["flash"]["max"], 1.0); // not the seconds that drop's flash lasts
        assert_eq!(of("screenhue")["schema"]["saturation"]["max"], 1.0);
        let range = |key: &str| all.iter().find_map(|e| e["schema"].get(key)).unwrap().clone();
        let bpm = json!({"type": "number", "min": 40.0, "max": 240.0, "step": 1.0, "when": {"follow": ["tempo"]}});
        assert_eq!(range("slow"), bpm);
        assert_eq!(range("fast"), bpm);
        assert_eq!(range("flash"), json!({"type": "number", "min": 0.1, "max": 2.0, "step": 0.05}));
        assert_eq!(range("build"), json!({"type": "number", "min": 0.5, "max": 10.0, "step": 0.5}));
        let music = all.iter().find(|e| e["name"] == "music").unwrap();
        assert_eq!(music["needs"], "audio");
        assert_eq!(music["params"]["color"], Value::Null);
        assert_eq!(music["schema"]["color"], json!({"type": "color", "optional": true}));
        assert_eq!(music["schema"]["delay"], json!({"type": "number", "min": 0.0, "max": 2.0, "step": 0.01}));
        assert_eq!(music["schema"]["step"], json!({"type": "number", "min": 1.0, "max": 180.0, "step": 1.0}));
        assert_eq!(music["params"]["floor"], 0.0); // dark between beats, as it always was
        assert!(!all.iter().any(|e| e["name"] == "beathue")); // it became a setting of music
        let blend = of("blend");
        assert_eq!(blend["schema"]["follow"], json!({"type": "choice", "options": ["balance", "brightness", "tempo"]}));
        assert_eq!(blend["params"]["follow"], "balance"); // what stereo was
                                                          // what only applies to one of them says so, so a front end can leave it out
        assert_eq!(blend["schema"]["width"]["when"], json!({"follow": ["balance", "brightness"]}));
        assert_eq!(blend["schema"]["sensitivity"]["when"], json!({"follow": ["tempo"]}));
        assert_eq!(blend["schema"]["release"].get("when"), None); // this one always applies
        for gone in ["stereo", "tempo", "centroid"] {
            assert!(!all.iter().any(|e| e["name"] == gone), "{gone}"); // they became settings of blend
        }
        let screen = all.iter().find(|e| e["name"] == "screen").unwrap();
        assert_eq!(screen["schema"]["saturation"]["max"], 3.0);
        assert_eq!(screen["also"], Value::Null);
        let both = all.iter().find(|e| e["name"] == "screensound").unwrap();
        assert_eq!((&both["needs"], &both["also"]), (&json!("screen"), &json!("audio")));
        assert_eq!(both["schema"]["shift"], json!({"type": "number", "min": 0.0, "max": 60.0, "step": 1.0}));
        let custom = all.iter().find(|e| e["name"] == "custom").unwrap();
        assert_eq!(custom["schema"]["steps"]["most"], 16);
        assert_eq!(custom["params"]["steps"][0]["ease"], "ease-in-out");
        let palette = all.iter().find(|e| e["name"] == "palette").unwrap();
        assert_eq!(palette["params"]["colors"], json!(["#ff0000", "#00ff00", "#0000ff"]));
        assert_eq!(all.iter().find(|e| e["name"] == "candle").unwrap()["params"]["color"], "#ff6e14");
    }
}
