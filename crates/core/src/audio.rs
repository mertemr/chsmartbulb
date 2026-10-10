//! Sound-reactive effects and the analysis behind them, as in `chsmartbulb.music`.
//!
//! [`Analyzer`] turns PCM into band levels and onset strengths; it runs wherever the
//! sound is captured (a phone's playback or microphone, a computer, an agent).
//! [`AudioSource`] holds what the effects read: the levels and beats, delayed so the
//! light lines up with an output that plays late, as a Bluetooth speaker does.

use std::collections::VecDeque;
use std::f64::consts::PI;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::color::{Color, BLUE, OFF, RED};
use crate::effects::{along, Effect};
use crate::error::{invalid, Result};

pub const RATE: u32 = 22050;
/// 23 ms per analysis step at [`RATE`].
pub const BLOCK: usize = 512;
pub const DEFAULT_SENSITIVITY: f64 = 0.5;
pub const MAX_DELAY: f64 = 2.0;

const BANDS_HZ: [(f64, f64); 3] = [(40.0, 250.0), (250.0, 2000.0), (2000.0, 8000.0)];
/// Seconds for the automatic gain to forget a loud passage.
const GAIN_HALF_LIFE: f64 = 4.0;
const SILENCE: f64 = 1e-4;
/// A microphone's sound must stand this many times above the room's noise.
const ROOM_MARGIN: f64 = 2.0;
/// Seconds for the estimate of that noise to double while nothing dips below it.
const ROOM_DOUBLING: f64 = 30.0;
/// Seconds; caps detection at 400 bpm.
const BEAT_GAP: f64 = 0.15;
/// A beat needs the bass above this fraction of its recent peak.
const BEAT_FLOOR: f64 = 0.05;
const ONSET_CAP: f64 = 100.0;
/// Seconds for the stereo position to settle.
const PAN_SMOOTHING: f64 = 0.15;
const DARK: f64 = 1.0 / 255.0;
const TEMPO_SLOW: Color = Color::rgb(255, 60, 0);
const TEMPO_FAST: Color = Color::rgb(0, 160, 255);
/// Seconds between beats that count as a tempo: 240 to 40 bpm.
const TEMPO_INTERVALS: (f64, f64) = (0.25, 1.5);
/// Share of a new interval in the estimate.
const TEMPO_BLEND: f64 = 0.3;
/// Brightness falls this much per second.
const TEMPO_RELEASE: f64 = 3.0;
/// Seconds; the long average the lull is measured against.
const DROP_SLOW: f64 = 8.0;
/// A lull: the short average under this share of the long one.
const LULL_RATIO: f64 = 0.35;
/// And the long one above this, so there was something to fall from.
const LULL_FLOOR: f64 = 0.05;
/// Seconds a lull must last.
const LULL_HOLD: f64 = 0.5;
/// Seconds `lulled` is remembered once the lull is over.
const LULL_MEMORY: f64 = 30.0;
/// A drop's beat must be this loud.
const DROP_ENERGY: f64 = 0.6;
const FLASH_HZ: f64 = 8.0;

/// A source of seconds on a steady time base.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// Seconds since the first call, on the monotonic clock.
pub fn monotonic() -> Clock {
    static START: OnceLock<Instant> = OnceLock::new();
    let start = *START.get_or_init(Instant::now);
    Arc::new(move || start.elapsed().as_secs_f64())
}

/// How far the bass must rise above its recent average to count as a beat.
///
/// 1.5 at the default sensitivity; 5 at 0 (only the hardest hits), barely above 1 at 1.
pub fn beat_ratio(sensitivity: f64) -> f64 {
    1.0 + 0.5 * 8.0f64.powf(1.0 - 2.0 * sensitivity)
}

/// Band loudness relative to the recent peak, each 0..1, and where the sound sits.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Levels {
    pub bass: f64,
    pub mid: f64,
    pub treble: f64,
    /// -1 all left, +1 all right.
    pub balance: f64,
}

impl Levels {
    fn loudness(&self) -> f64 {
        self.bass.max(self.mid).max(self.treble)
    }
}

struct Published {
    held: VecDeque<(f64, Levels, bool)>,
    last_onset: f64,
    delay: f64,
    sensitivity: f64,
    levels: Levels,
    beats: u64,
    last_beat: f64,
}

/// Levels and beats as the effects see them.
///
/// `delay` holds them back by that many seconds: the capture hears the sound before
/// a Bluetooth speaker plays it, so without a delay the light runs ahead of what you
/// hear. `sensitivity` (0..1) decides which onsets count as beats.
pub struct AudioSource {
    clock: Clock,
    state: Mutex<Published>,
}

/// What an effect reads from an [`AudioSource`] for one frame.
#[derive(Debug, Clone, Copy)]
pub struct Heard {
    pub levels: Levels,
    pub beats: u64,
    pub last_beat: f64,
    pub now: f64,
}

impl AudioSource {
    pub fn new(clock: Clock) -> Arc<Self> {
        Arc::new(Self {
            clock,
            state: Mutex::new(Published {
                held: VecDeque::new(),
                last_onset: f64::NEG_INFINITY,
                delay: 0.0,
                sensitivity: DEFAULT_SENSITIVITY,
                levels: Levels::default(),
                beats: 0,
                last_beat: f64::NEG_INFINITY,
            }),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Published> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn now(&self) -> f64 {
        (self.clock)()
    }

    pub fn set_delay(&self, delay: f64) -> Result<()> {
        if !(0.0..=MAX_DELAY).contains(&delay) {
            return Err(invalid(format!("delay must be within 0..{MAX_DELAY} seconds")));
        }
        self.lock().delay = delay;
        Ok(())
    }

    pub fn set_sensitivity(&self, sensitivity: f64) -> Result<()> {
        if !(0.0..=1.0).contains(&sensitivity) {
            return Err(invalid("sensitivity must be within 0..1"));
        }
        self.lock().sensitivity = sensitivity;
        Ok(())
    }

    /// Take one analysed block; `onset` is the bass relative to its recent average.
    pub fn publish(&self, levels: Levels, onset: f64) {
        let now = self.now();
        let mut state = self.lock();
        let beat = onset >= beat_ratio(state.sensitivity) && now - state.last_onset > BEAT_GAP;
        if beat {
            state.last_onset = now;
        }
        let due = now + state.delay;
        state.held.push_back((due, levels, beat));
        Self::release(&mut state, now);
    }

    fn release(state: &mut Published, now: f64) {
        while state.held.front().is_some_and(|(due, _, _)| *due <= now) {
            let (due, levels, beat) = state.held.pop_front().expect("checked above");
            state.levels = levels;
            if beat {
                state.beats += 1;
                state.last_beat = due;
            }
        }
    }

    /// Forget what was pending and go silent, for when the feed stops.
    pub fn clear(&self) {
        let mut state = self.lock();
        state.held.clear();
        state.levels = Levels::default();
    }

    pub fn heard(&self) -> Heard {
        let now = self.now();
        let mut state = self.lock();
        Self::release(&mut state, now); // a delayed block falls due even when no new one arrives
        Heard { levels: state.levels, beats: state.beats, last_beat: state.last_beat, now }
    }
}

/// Analyses signed 16-bit PCM into band levels and onsets, block by block.
///
/// A microphone hears the room as well as the music, so with `mic` the steady
/// noise of the room is estimated and taken off each band.
pub struct Analyzer {
    rate: u32,
    block: usize,
    channels: usize,
    mic: bool,
    window: Vec<f64>,
    bins: [(usize, usize); 3],
    decay: f64,
    room_rise: f64,
    room: [f64; 3],
    peaks: [f64; 3],
    bass_average: f64,
    pending: Vec<i16>,
    fft: Arc<dyn Fft<f64>>,
    scratch: Vec<Complex<f64>>,
}

impl Analyzer {
    /// Size the analysis for `rate`; without `block`, pick one of about 23 ms or more.
    pub fn new(rate: u32, channels: usize, mic: bool, block: Option<usize>) -> Self {
        let block = block.unwrap_or_else(|| {
            let mut block = BLOCK;
            while rate as f64 / block as f64 > 60.0 {
                block *= 2; // keep the bins near 45 Hz wide so the bass band stays resolved
            }
            block
        });
        let hz_per_bin = rate as f64 / block as f64;
        let edge = |hz: f64| crate::color::round(hz / hz_per_bin) as usize;
        let bins = BANDS_HZ.map(|(lo, hi)| (edge(lo).max(1), edge(hi)));
        // numpy.hanning
        let window = (0..block).map(|n| 0.5 - 0.5 * (2.0 * PI * n as f64 / (block as f64 - 1.0)).cos()).collect();
        let seconds = block as f64 / rate as f64;
        Self {
            rate,
            block,
            channels: channels.max(1),
            mic,
            window,
            bins,
            decay: 0.5f64.powf(seconds / GAIN_HALF_LIFE),
            room_rise: 2.0f64.powf(seconds / ROOM_DOUBLING),
            room: [f64::INFINITY; 3],
            peaks: [0.0; 3],
            bass_average: 0.0,
            pending: Vec::new(),
            fft: FftPlanner::new().plan_fft_forward(block),
            scratch: vec![Complex::default(); block],
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn block(&self) -> usize {
        self.block
    }

    /// Consume interleaved samples; returns each analysed block as levels and onset strength.
    pub fn feed(&mut self, pcm: &[i16]) -> Vec<(Levels, f64)> {
        self.pending.extend_from_slice(pcm);
        let size = self.block * self.channels;
        let mut blocks = Vec::new();
        while self.pending.len() >= size {
            let samples: Vec<i16> = self.pending.drain(..size).collect();
            blocks.push(self.analyse(&samples));
        }
        blocks
    }

    /// Like [`feed`](Self::feed), from little-endian bytes as most capture APIs deliver them.
    pub fn feed_bytes(&mut self, pcm: &[u8]) -> Vec<(Levels, f64)> {
        let samples: Vec<i16> = pcm.as_chunks::<2>().0.iter().map(|pair| i16::from_le_bytes(*pair)).collect();
        self.feed(&samples)
    }

    fn balance(&self, samples: &[i16]) -> f64 {
        if self.channels < 2 {
            return 0.0;
        }
        let rms = |side: usize| {
            let sum: f64 =
                samples.chunks_exact(self.channels).map(|frame| (frame[side] as f64 / 32768.0).powi(2)).sum();
            (sum / self.block as f64).sqrt()
        };
        let (left, right) = (rms(0), rms(1));
        let total = left + right;
        if total > SILENCE {
            (right - left) / total
        } else {
            0.0
        }
    }

    fn analyse(&mut self, samples: &[i16]) -> (Levels, f64) {
        let channels = self.channels;
        for (i, frame) in samples.chunks_exact(channels).enumerate() {
            let mean = frame.iter().map(|&s| s as f64 / 32768.0).sum::<f64>() / channels as f64;
            self.scratch[i] = Complex::new(mean * self.window[i], 0.0);
        }
        self.fft.process(&mut self.scratch);
        let half = self.block as f64 / 2.0;
        let spectrum_len = self.block / 2 + 1;
        let mut values = [0.0f64; 3];
        for (band, &(lo, hi)) in self.bins.iter().enumerate() {
            let hi = hi.min(spectrum_len);
            values[band] = if hi > lo {
                self.scratch[lo..hi].iter().map(|c| c.norm() / half).sum::<f64>() / (hi - lo) as f64
            } else {
                0.0
            };
        }
        if self.mic {
            for (band, value) in values.iter_mut().enumerate() {
                *value = self.above_room(band, *value);
            }
        }
        let mut levels = [0.0f64; 3];
        for (i, &value) in values.iter().enumerate() {
            let heard = value > SILENCE;
            // the gain holds through a pause, or the first faint sound after it would read as full level
            if heard {
                self.peaks[i] = value.max(self.peaks[i] * self.decay);
                levels[i] = value / self.peaks[i];
            }
        }
        let bass = values[0];
        let mut onset = 0.0;
        if levels[0] >= BEAT_FLOOR {
            // something faint after a loud passage is not a beat
            onset = if self.bass_average > 0.0 { ONSET_CAP.min(bass / self.bass_average) } else { ONSET_CAP };
        }
        // fast enough to catch up with a sustained note before the gap allows another beat
        self.bass_average += 0.25 * (bass - self.bass_average);
        let result = Levels { bass: levels[0], mid: levels[1], treble: levels[2], balance: self.balance(samples) };
        (result, onset)
    }

    /// What of `value` stands out of the room's noise, the least the band has held lately.
    fn above_room(&mut self, band: usize, value: f64) -> f64 {
        self.room[band] = value.min(self.room[band].max(SILENCE / 10.0) * self.room_rise);
        (value - ROOM_MARGIN * self.room[band]).max(0.0)
    }
}

/// Flash on every beat and glow with the bass in between.
///
/// Without a `color` the hue steps to a new one on each beat.
pub fn music_pulse(
    source: Arc<AudioSource>,
    color: Option<Color>,
    decay: f64,
    delay: f64,
    sensitivity: f64,
) -> Result<Effect> {
    source.set_delay(delay)?;
    source.set_sensitivity(sensitivity)?;
    Ok(Box::new(move |_t| {
        let heard = source.heard();
        let flash = (-decay * (heard.now - heard.last_beat)).exp();
        let level = flash.max(0.6 * heard.levels.bass).min(1.0);
        if level < DARK {
            return OFF; // scaled() never rounds a lit channel to zero, silence should be dark
        }
        let base = color.unwrap_or_else(|| Color::from_hsv(heard.beats as f64 * 47.0, 1.0, 1.0));
        base.scaled(level)
    }))
}

/// Turn the colour by `step` degrees on every beat; flash and glow with the bass.
///
/// Between beats the light falls to `floor` of its brightness, never fully dark.
pub fn music_beathue(
    source: Arc<AudioSource>,
    step: f64,
    decay: f64,
    floor: f64,
    saturation: f64,
    delay: f64,
    sensitivity: f64,
) -> Result<Effect> {
    if !(1.0..=180.0).contains(&step) {
        return Err(invalid("step must be within 1..180 degrees"));
    }
    if decay <= 0.0 {
        return Err(invalid("decay must be positive"));
    }
    if !(0.0..=1.0).contains(&floor) {
        return Err(invalid("floor must be within 0..1"));
    }
    if !(0.0..=1.0).contains(&saturation) {
        return Err(invalid("saturation must be within 0..1"));
    }
    source.set_delay(delay)?;
    source.set_sensitivity(sensitivity)?;
    Ok(Box::new(move |_t| {
        let heard = source.heard();
        let flash = (-decay * (heard.now - heard.last_beat)).exp();
        let level = floor + (1.0 - floor) * flash.max(0.6 * heard.levels.bass);
        if level < DARK {
            return OFF;
        }
        Color::from_hsv(heard.beats as f64 * step, saturation, 1.0).scaled(level)
    }))
}

/// Warm for slow music, cool for fast; brightness follows the loudness.
///
/// The tempo is estimated from the gaps between beats. `slow` and `fast` (bpm) are the
/// tempos shown as the warm and the cool end, `smoothing` the seconds the colour takes to settle.
pub fn music_tempo(
    source: Arc<AudioSource>,
    slow: f64,
    fast: f64,
    smoothing: f64,
    delay: f64,
    sensitivity: f64,
) -> Result<Effect> {
    if slow >= fast {
        return Err(invalid("slow must be below fast"));
    }
    if smoothing < 0.0 {
        return Err(invalid("smoothing must not be negative"));
    }
    source.set_delay(delay)?;
    source.set_sensitivity(sensitivity)?;
    let mut estimate: Option<f64> = None; // seconds between beats
    let mut previous: Option<f64> = None;
    let mut seen = source.heard().beats;
    let mut position = 0.5f64;
    let mut shown = 0.0f64;
    let mut last = 0.0;
    Ok(Box::new(move |t| {
        let step = (t - last).max(0.0);
        last = t;
        let heard = source.heard();
        if heard.beats != seen {
            if heard.beats > seen {
                if let Some(before) = previous {
                    let interval = (heard.last_beat - before) / (heard.beats - seen) as f64;
                    if (TEMPO_INTERVALS.0..=TEMPO_INTERVALS.1).contains(&interval) {
                        estimate = Some(match estimate {
                            Some(old) => (1.0 - TEMPO_BLEND) * old + TEMPO_BLEND * interval,
                            None => interval,
                        });
                    }
                }
            }
            seen = heard.beats;
            previous = Some(heard.last_beat);
        }
        let target = match estimate {
            Some(interval) => ((60.0 / interval - slow) / (fast - slow)).clamp(0.0, 1.0),
            None => 0.5,
        };
        if smoothing > 0.0 {
            position += (target - position) * (1.0 - (-step / smoothing).exp());
        } else {
            position = target;
        }
        shown = heard.levels.loudness().max(shown - TEMPO_RELEASE * step);
        let level = shown * shown;
        if level < DARK {
            return OFF;
        }
        TEMPO_SLOW.mix(&TEMPO_FAST, position).scaled(level)
    }))
}

/// Blend between two colours by where the sound's weight lies between bass and treble.
///
/// The weight of real music seldom passes the middle, so `width` stretches it.
/// Brightness follows the loudness.
pub fn music_centroid(
    source: Arc<AudioSource>,
    low: Color,
    high: Color,
    width: f64,
    release: f64,
    delay: f64,
) -> Result<Effect> {
    if width <= 0.0 {
        return Err(invalid("width must be positive"));
    }
    source.set_delay(delay)?;
    let mut shown = 0.0f64;
    let mut position = 0.0f64;
    let mut last = 0.0;
    Ok(Box::new(move |t| {
        let step = (t - last).max(0.0);
        last = t;
        let levels = source.heard().levels;
        let total = levels.bass + levels.mid + levels.treble;
        let faded = (shown - release * step).max(0.0);
        if total > 0.0 {
            // silence says nothing about the weight, keep the last one
            let target = (width * (0.5 * levels.mid + levels.treble) / total).clamp(0.0, 1.0);
            if faded * faded < DARK {
                position = target;
            } else {
                position += (target - position) * (1.0 - (-step / PAN_SMOOTHING).exp());
            }
        }
        shown = levels.loudness().max(faded);
        let level = shown * shown;
        if level >= DARK {
            low.mix(&high, position).scaled(level)
        } else {
            OFF
        }
    }))
}

/// Open up as the music builds and flash when it comes back in after a lull.
///
/// `build` is the seconds the light takes to follow the energy, `flash` how long the flash
/// lasts. The thresholds are tuned on synthetic music, not on real tracks.
pub fn music_drop(
    source: Arc<AudioSource>,
    color: Color,
    flash: f64,
    build: f64,
    delay: f64,
    sensitivity: f64,
) -> Result<Effect> {
    if flash <= 0.0 {
        return Err(invalid("flash must be positive"));
    }
    if build <= 0.0 {
        return Err(invalid("build must be positive"));
    }
    source.set_delay(delay)?;
    source.set_sensitivity(sensitivity)?;
    let mut fast = 0.0f64;
    let mut slow = 0.0f64;
    let mut lull = 0.0f64;
    let mut since = 0.0f64;
    let mut lulled = false;
    let mut seen = source.heard().beats;
    let mut began = f64::NEG_INFINITY;
    let mut last = 0.0;
    Ok(Box::new(move |t| {
        let step = (t - last).max(0.0);
        last = t;
        let heard = source.heard();
        let energy = heard.levels.loudness();
        fast += (energy - fast) * (1.0 - (-step / build).exp());
        slow += (energy - slow) * (1.0 - (-step / DROP_SLOW).exp());
        if slow > LULL_FLOOR && fast < LULL_RATIO * slow {
            lull += step;
            since = 0.0;
            if lull >= LULL_HOLD {
                lulled = true;
            }
        } else {
            lull = 0.0;
            if lulled {
                since += step;
                if since >= LULL_MEMORY {
                    // the lull is long forgotten: a beat now is not a drop
                    lulled = false;
                    since = 0.0;
                }
            }
        }
        let arrived = heard.beats != seen;
        seen = heard.beats;
        if arrived && lulled && energy >= DROP_ENERGY {
            lulled = false;
            since = 0.0;
            began = t;
        }
        if t - began < flash {
            return if ((t - began) * FLASH_HZ).rem_euclid(1.0) < 0.5 { color } else { OFF };
        }
        let glow = 0.15 * (slow / LULL_FLOOR).min(1.0) + 0.6 * fast;
        if glow >= DARK {
            color.scaled(glow)
        } else {
            OFF
        }
    }))
}

/// A calm colour that breathes every `period` seconds, with `accent` flashing on the beats.
///
/// Never dark: in silence it is only the breathing.
pub fn music_ambient(
    source: Arc<AudioSource>,
    base: Color,
    accent: Color,
    period: f64,
    decay: f64,
    delay: f64,
    sensitivity: f64,
) -> Result<Effect> {
    if period <= 0.0 {
        return Err(invalid("period must be positive"));
    }
    if decay <= 0.0 {
        return Err(invalid("decay must be positive"));
    }
    source.set_delay(delay)?;
    source.set_sensitivity(sensitivity)?;
    Ok(Box::new(move |t| {
        let heard = source.heard();
        let swell = 0.5 - 0.5 * (2.0 * PI * t / period).cos();
        let flash = (-decay * (heard.now - heard.last_beat)).exp();
        base.scaled(0.25 + 0.25 * swell).mix(&accent, flash)
    }))
}

/// Bass drives red, mids green and treble blue; `release` is the fall rate per second.
pub fn music_spectrum(source: Arc<AudioSource>, release: f64, delay: f64) -> Result<Effect> {
    source.set_delay(delay)?;
    let mut shown = [0.0f64; 3];
    let mut last = 0.0;
    Ok(Box::new(move |t| {
        let fall = release * (t - last).max(0.0);
        last = t;
        let levels = source.heard().levels;
        for (shown, target) in shown.iter_mut().zip([levels.bass, levels.mid, levels.treble]) {
            *shown = target.max(*shown - fall);
        }
        // squared for contrast: quiet bands stay dim
        let channel = |value: f64| crate::color::round(255.0 * value * value).clamp(0.0, 255.0) as u8;
        Color::rgb(channel(shown[0]), channel(shown[1]), channel(shown[2]))
    }))
}

/// One colour whose brightness follows the loudness; `floor` is kept in silence.
pub fn music_volume(source: Arc<AudioSource>, color: Color, release: f64, floor: f64, delay: f64) -> Result<Effect> {
    source.set_delay(delay)?;
    if !(0.0..=1.0).contains(&floor) {
        return Err(invalid("floor must be within 0..1"));
    }
    let mut shown = 0.0f64;
    let mut last = 0.0;
    Ok(Box::new(move |t| {
        shown = source.heard().levels.loudness().max(shown - release * (t - last).max(0.0));
        last = t;
        let level = floor + (1.0 - floor) * shown * shown;
        if level >= DARK {
            color.scaled(level)
        } else {
            OFF
        }
    }))
}

/// Blend between two colours by where the sound sits; brightness follows the loudness.
///
/// Music rarely leans far to one side, so `width` stretches the measured position.
pub fn music_stereo(
    source: Arc<AudioSource>,
    left: Color,
    right: Color,
    width: f64,
    release: f64,
    delay: f64,
) -> Result<Effect> {
    source.set_delay(delay)?;
    if width <= 0.0 {
        return Err(invalid("width must be positive"));
    }
    let mut shown = 0.0f64;
    let mut position = 0.5f64;
    let mut last = 0.0;
    Ok(Box::new(move |t| {
        let step = (t - last).max(0.0);
        last = t;
        let levels = source.heard().levels;
        let loudness = levels.loudness();
        let faded = (shown - release * step).max(0.0);
        if loudness > 0.0 {
            // silence says nothing about the position, keep the last one
            let target = (0.5 + 0.5 * width * levels.balance).clamp(0.0, 1.0);
            if faded * faded < DARK {
                position = target; // out of the dark a sound starts where it is
            } else {
                position += (target - position) * (1.0 - (-step / PAN_SMOOTHING).exp());
            }
        }
        shown = loudness.max(faded);
        let level = shown * shown;
        if level >= DARK {
            left.mix(&right, position).scaled(level)
        } else {
            OFF
        }
    }))
}

/// The bass in one colour and the treble in another, each as bright as its band is loud.
///
/// The two add up, so a kick and a hi-hat at once show both colours mixed.
pub fn music_bands(source: Arc<AudioSource>, low: Color, high: Color, release: f64, delay: f64) -> Result<Effect> {
    source.set_delay(delay)?;
    if release <= 0.0 {
        return Err(invalid("release must be positive"));
    }
    let mut shown = [0.0f64; 2];
    let mut last = 0.0;
    Ok(Box::new(move |t| {
        let fall = release * (t - last).max(0.0);
        last = t;
        let levels = source.heard().levels;
        shown = [levels.bass.max(shown[0] - fall), levels.treble.max(shown[1] - fall)];
        let lit = |color: Color, level: f64| if level * level >= DARK { color.scaled(level * level) } else { OFF };
        let (low, high) = (lit(low, shown[0]), lit(high, shown[1]));
        Color::rgbw(
            low.r.saturating_add(high.r),
            low.g.saturating_add(high.g),
            low.b.saturating_add(high.b),
            low.w.saturating_add(high.w),
        )
    }))
}

/// Default colours of the bands effect.
pub const BANDS_LOW: Color = Color::rgb(255, 0, 40);
pub const BANDS_HIGH: Color = Color::rgb(0, 120, 255);

/// How loud the music has been lately picks the colour: the first of `colors` for a calm
/// passage, the last for an intense one. The brightness follows the sound as it is now.
///
/// `smoothing` is how many seconds of music the colour looks back on.
pub fn music_energy(
    source: Arc<AudioSource>,
    colors: &[Color],
    smoothing: f64,
    floor: f64,
    delay: f64,
) -> Result<Effect> {
    source.set_delay(delay)?;
    if colors.is_empty() {
        return Err(invalid("colors must not be empty"));
    }
    if smoothing <= 0.0 {
        return Err(invalid("smoothing must be positive"));
    }
    if !(0.0..=1.0).contains(&floor) {
        return Err(invalid("floor must be within 0..1"));
    }
    let colors = colors.to_vec();
    let mut energy = 0.0f64;
    let mut shown = 0.0f64;
    let mut last = 0.0;
    Ok(Box::new(move |t| {
        let step = (t - last).max(0.0);
        last = t;
        let loudness = source.heard().levels.loudness();
        energy += (loudness - energy) * (1.0 - (-step / smoothing).exp());
        shown = loudness.max(shown - ENERGY_RELEASE * step);
        let level = floor + (1.0 - floor) * shown * shown;
        if level < DARK {
            return OFF;
        }
        // the levels are relative to the recent peak, so their average seldom leaves this span
        let place = (energy - ENERGY_SPAN.0) / (ENERGY_SPAN.1 - ENERGY_SPAN.0);
        along(&colors, place).scaled(level)
    }))
}

/// How fast the brightness of the energy effect falls, per second.
const ENERGY_RELEASE: f64 = 3.0;
/// The average loudness shown as the first and as the last colour.
const ENERGY_SPAN: (f64, f64) = (0.15, 0.7);
/// Default colours of the energy effect, calm to intense.
pub const ENERGY_COLORS: [Color; 3] = [Color::rgb(0, 60, 255), Color::rgb(200, 0, 255), Color::rgb(255, 30, 0)];

/// A colour that wanders around the wheel, swelling and sinking with the music instead of
/// flashing on it: no beats, only how loud it is, taken slowly.
///
/// `smoothing` is how many seconds the brightness takes to follow the sound, both ways.
pub fn music_chill(
    source: Arc<AudioSource>,
    period: f64,
    saturation: f64,
    floor: f64,
    smoothing: f64,
    delay: f64,
) -> Result<Effect> {
    source.set_delay(delay)?;
    if period <= 0.0 {
        return Err(invalid("period must be positive"));
    }
    if !(0.0..=1.0).contains(&saturation) {
        return Err(invalid("saturation must be within 0..1"));
    }
    if !(0.0..=1.0).contains(&floor) {
        return Err(invalid("floor must be within 0..1"));
    }
    if smoothing < 0.0 {
        return Err(invalid("smoothing must not be negative"));
    }
    let mut shown = 0.0f64;
    let mut last = 0.0;
    Ok(Box::new(move |t| {
        let step = (t - last).max(0.0);
        last = t;
        let loudness = source.heard().levels.loudness();
        let blend = if smoothing > 0.0 { 1.0 - (-step / smoothing).exp() } else { 1.0 };
        shown += (loudness - shown) * blend;
        let level = floor + (1.0 - floor) * shown;
        if level < DARK {
            return OFF;
        }
        Color::from_hsv(360.0 * t / period, saturation, 1.0).scaled(level)
    }))
}

/// Default colours of the stereo effect.
pub const STEREO_LEFT: Color = BLUE;
pub const STEREO_RIGHT: Color = RED;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::WHITE;
    use crate::effects::WARM;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn manual_clock() -> (Clock, Arc<AtomicU64>) {
        let now = Arc::new(AtomicU64::new(0f64.to_bits()));
        let read = now.clone();
        (Arc::new(move || f64::from_bits(read.load(Ordering::SeqCst))), now)
    }

    fn set(now: &AtomicU64, value: f64) {
        now.store(value.to_bits(), Ordering::SeqCst);
    }

    #[test]
    fn beat_ratio_spans_the_sensitivity() {
        assert!((beat_ratio(0.5) - 1.5).abs() < 1e-12);
        assert!((beat_ratio(0.0) - 5.0).abs() < 1e-12);
    }

    #[test]
    fn delay_holds_levels_back() {
        let (clock, now) = manual_clock();
        let source = AudioSource::new(clock);
        source.set_delay(0.5).unwrap();
        source.publish(Levels { bass: 1.0, ..Levels::default() }, 10.0);
        assert_eq!(source.heard().levels.bass, 0.0);
        set(&now, 0.6);
        let heard = source.heard();
        assert_eq!(heard.levels.bass, 1.0);
        assert_eq!(heard.beats, 1);
        assert!((heard.last_beat - 0.5).abs() < 1e-12);
    }

    #[test]
    fn beats_need_a_gap() {
        let (clock, now) = manual_clock();
        let source = AudioSource::new(clock);
        source.publish(Levels::default(), 10.0);
        set(&now, 0.1);
        source.publish(Levels::default(), 10.0);
        set(&now, 0.3);
        source.publish(Levels::default(), 10.0);
        assert_eq!(source.heard().beats, 2);
    }

    #[test]
    fn analyzer_hears_bass_and_beats() {
        let mut analyzer = Analyzer::new(RATE, 1, false, None);
        let silence = vec![0i16; BLOCK * 4];
        assert!(analyzer.feed(&silence).iter().all(|(levels, onset)| levels.bass == 0.0 && *onset == 0.0));
        let tone: Vec<i16> =
            (0..BLOCK * 2).map(|i| ((2.0 * PI * 100.0 * i as f64 / RATE as f64).sin() * 16000.0) as i16).collect();
        let blocks = analyzer.feed(&tone);
        assert_eq!(blocks.len(), 2);
        let (levels, onset) = blocks[0];
        assert!(levels.bass > 0.99, "{levels:?}");
        assert!(levels.treble < 0.5, "{levels:?}");
        assert_eq!(onset, ONSET_CAP);
        // a steady note is no longer a beat
        assert!(blocks[1].1 < 5.0);
    }

    #[test]
    fn stereo_balance_reads_the_louder_side() {
        let mut analyzer = Analyzer::new(RATE, 2, false, None);
        let pcm: Vec<i16> = (0..BLOCK)
            .flat_map(|i| {
                let s = ((2.0 * PI * 440.0 * i as f64 / RATE as f64).sin() * 10000.0) as i16;
                [s / 4, s]
            })
            .collect();
        let (levels, _) = analyzer.feed(&pcm)[0];
        assert!(levels.balance > 0.5, "{levels:?}");
    }

    #[test]
    fn pulse_flashes_and_goes_dark() {
        let (clock, now) = manual_clock();
        let source = AudioSource::new(clock);
        let mut effect = music_pulse(source.clone(), Some(RED), 5.0, 0.0, 0.5).unwrap();
        assert_eq!(effect(0.0), OFF);
        source.publish(Levels::default(), 10.0);
        assert_eq!(effect(0.0), RED);
        set(&now, 3.0);
        assert_eq!(effect(3.0), OFF);
    }

    #[test]
    fn beathue_steps_the_hue_and_keeps_a_floor() {
        let (clock, now) = manual_clock();
        let source = AudioSource::new(clock);
        let mut effect = music_beathue(source.clone(), 120.0, 5.0, 0.2, 1.0, 0.0, 0.5).unwrap();
        assert_eq!(effect(0.0), Color::rgb(51, 0, 0)); // never beat and silent: the floor, in red
        set(&now, 10.0);
        source.publish(Levels::default(), 10.0); // a beat, the bass silent
        assert_eq!(effect(0.0), Color::rgb(0, 255, 0));
        set(&now, 13.0);
        assert_eq!(effect(3.0), Color::rgb(0, 51, 0));
        source.publish(Levels::default(), 10.0);
        assert_eq!(effect(3.0), Color::rgb(0, 0, 255));
    }

    #[test]
    fn beathue_refuses_what_does_not_fit() {
        let (clock, _) = manual_clock();
        let source = AudioSource::new(clock);
        for (step, decay, floor, saturation, delay, sensitivity) in [
            (0.0, 5.0, 0.1, 1.0, 0.0, 0.5),
            (200.0, 5.0, 0.1, 1.0, 0.0, 0.5),
            (47.0, 0.0, 0.1, 1.0, 0.0, 0.5),
            (47.0, 5.0, 2.0, 1.0, 0.0, 0.5),
            (47.0, 5.0, 0.1, -1.0, 0.0, 0.5),
            (47.0, 5.0, 0.1, 1.0, 5.0, 0.5),
            (47.0, 5.0, 0.1, 1.0, 0.0, 2.0),
        ] {
            assert!(music_beathue(source.clone(), step, decay, floor, saturation, delay, sensitivity).is_err());
        }
    }

    #[test]
    fn tempo_colours_by_how_fast_the_beats_come() {
        let (clock, now) = manual_clock();
        let source = AudioSource::new(clock);
        let loud = Levels { bass: 1.0, ..Levels::default() };
        let mut effect = music_tempo(source.clone(), 80.0, 120.0, 0.0, 0.0, 0.5).unwrap();
        set(&now, 10.0);
        source.publish(loud, 10.0);
        effect(0.0); // the first beat gives no interval yet
        set(&now, 10.5);
        source.publish(loud, 10.0);
        assert_eq!(effect(0.5), Color::rgb(0, 160, 255)); // 120 bpm is the fast end
        set(&now, 10.7);
        source.publish(loud, 10.0); // 0.2 s apart is no tempo
        assert_eq!(effect(0.7), Color::rgb(0, 160, 255));
        set(&now, 12.5);
        source.publish(loud, 10.0); // neither is 2 s
        assert_eq!(effect(2.5), Color::rgb(0, 160, 255));
        set(&now, 30.0);
        assert_eq!(effect(20.0), Color::rgb(0, 160, 255)); // without beats the estimate holds

        let mut slow = music_tempo(source.clone(), 80.0, 120.0, 0.0, 0.0, 0.5).unwrap();
        set(&now, 40.0);
        source.publish(loud, 10.0);
        slow(0.0);
        set(&now, 41.0);
        source.publish(loud, 10.0);
        assert_eq!(slow(1.0), Color::rgb(255, 60, 0)); // 60 bpm is below slow
    }

    #[test]
    fn tempo_reads_beats_that_arrive_between_two_frames() {
        let (clock, now) = manual_clock();
        let source = AudioSource::new(clock);
        let loud = Levels { bass: 1.0, ..Levels::default() };
        let mut effect = music_tempo(source.clone(), 80.0, 120.0, 0.0, 0.0, 0.5).unwrap();
        set(&now, 10.0);
        source.publish(loud, 10.0);
        effect(0.0);
        set(&now, 10.5);
        source.publish(loud, 10.0);
        set(&now, 11.0);
        source.publish(loud, 10.0); // two beats before the next frame, 0.5 s apart
        assert_eq!(effect(1.0), Color::rgb(0, 160, 255));
    }

    #[test]
    fn tempo_is_dark_in_silence_and_refuses_what_does_not_fit() {
        let (clock, _) = manual_clock();
        let source = AudioSource::new(clock);
        let mut effect = music_tempo(source.clone(), 80.0, 160.0, 2.0, 0.0, 0.5).unwrap();
        assert_eq!(effect(0.0), OFF);
        for (slow, fast, smoothing, sensitivity) in
            [(120.0, 120.0, 2.0, 0.5), (160.0, 80.0, 2.0, 0.5), (80.0, 160.0, -1.0, 0.5), (80.0, 160.0, 2.0, 2.0)]
        {
            assert!(music_tempo(source.clone(), slow, fast, smoothing, 0.0, sensitivity).is_err());
        }
    }

    #[test]
    fn tempo_ignores_beats_from_before_it_started() {
        let (clock, now) = manual_clock();
        let source = AudioSource::new(clock);
        let loud = Levels { bass: 1.0, ..Levels::default() };
        set(&now, 10.0);
        source.publish(loud, 10.0); // beat before the effect exists
        let mut effect = music_tempo(source.clone(), 80.0, 120.0, 0.0, 0.0, 0.5).unwrap();
        effect(0.0); // trigger the first frame (no beat change yet, seen matches heard.beats)
        set(&now, 10.5);
        source.publish(loud, 10.0); // first beat change seen by effect: previous is set to 10.0, no interval calculated
        assert_ne!(effect(0.5), Color::rgb(0, 160, 255)); // not fast yet (first interval needs two beats after effect started)
        set(&now, 11.0);
        source.publish(loud, 10.0); // second beat change: now interval = (11.0 - 10.5) / 1 = 0.5s
        assert_eq!(effect(1.0), Color::rgb(0, 160, 255)); // 120 bpm is the fast end
    }

    #[test]
    fn centroid_blends_by_where_the_weight_lies() {
        let (clock, _) = manual_clock();
        let source = AudioSource::new(clock);
        let mut effect = music_centroid(source.clone(), RED, BLUE, 1.0, 2.0, 0.0).unwrap();
        assert_eq!(effect(0.0), OFF); // silent: dark, and no division by zero
        source.publish(Levels { bass: 1.0, ..Levels::default() }, 0.0);
        assert_eq!(effect(1.0), Color::rgb(255, 0, 0));
        source.publish(Levels { treble: 1.0, ..Levels::default() }, 0.0);
        assert_eq!(effect(1.05), Color::rgb(183, 0, 72));
        source.publish(Levels::default(), 0.0);
        assert_eq!(effect(2.0), OFF); // silence: step 0.95 makes faded 0, position untouched
        source.publish(Levels { treble: 1.0, ..Levels::default() }, 0.0);
        assert_eq!(effect(2.05), Color::rgb(0, 0, 255)); // out of the dark, position jumps to target 1.0 at once
        source.publish(Levels::default(), 0.0);
        assert_eq!(effect(2.15), Color::rgb(0, 0, 163)); // silence: faded = 0.8, level 0.64, position stays 1.0
    }

    #[test]
    fn centroid_refuses_what_does_not_fit() {
        let (clock, _) = manual_clock();
        let source = AudioSource::new(clock);
        assert!(music_centroid(source.clone(), RED, BLUE, 0.0, 3.0, 0.0).is_err());
        assert!(music_centroid(source.clone(), RED, BLUE, 2.0, 3.0, 5.0).is_err());
    }

    fn advance(effect: &mut Effect, t: &mut f64, seconds: f64) -> Color {
        let mut frame = OFF;
        for _ in 0..(seconds / 0.05).round() as usize {
            *t += 0.05;
            frame = effect(*t);
        }
        frame
    }

    #[test]
    fn drop_flashes_when_the_music_comes_back_after_a_lull() {
        let (clock, now) = manual_clock();
        let source = AudioSource::new(clock);
        let loud = Levels { bass: 1.0, ..Levels::default() };
        let green = Color::rgb(0, 255, 0);
        let mut effect = music_drop(source.clone(), green, 0.4, 3.0, 0.0, 0.5).unwrap();
        let mut t = 0.0;
        source.publish(loud, 0.0);
        advance(&mut effect, &mut t, 20.0); // steady loud music
        set(&now, t);
        source.publish(loud, 10.0); // a beat
        t += 0.05;
        assert!(effect(t).g < 255); // no drop in steady music
        set(&now, t);
        source.publish(Levels::default(), 0.0);
        let quiet = advance(&mut effect, &mut t, 8.0);
        assert!(0 < quiet.g && quiet.g < 100); // a dim glow in the lull
        t += 0.05;
        set(&now, t);
        source.publish(loud, 10.0);
        let began = t;
        assert_eq!(effect(t), green); // the drop: flash on
        assert_eq!(effect(began + 0.07), OFF); // and off, at 8 Hz
        assert!(effect(began + 0.5).g < 255);
    }

    #[test]
    fn drop_ignores_music_that_comes_back_gently() {
        let (clock, now) = manual_clock();
        let source = AudioSource::new(clock);
        let loud = Levels { bass: 1.0, ..Levels::default() };
        let mut effect = music_drop(source.clone(), Color::rgb(0, 255, 0), 0.4, 3.0, 0.0, 0.5).unwrap();
        let mut t = 0.0;
        source.publish(loud, 0.0);
        advance(&mut effect, &mut t, 20.0);
        source.publish(Levels::default(), 0.0);
        advance(&mut effect, &mut t, 8.0); // a lull
        source.publish(Levels { bass: 0.3, ..Levels::default() }, 0.0);
        advance(&mut effect, &mut t, 45.0); // back, but quietly: the lull is forgotten after 30 s
        t += 0.05;
        set(&now, t);
        source.publish(loud, 10.0);
        assert!(effect(t).g < 255); // an ordinary beat much later is not a drop
    }

    #[test]
    fn drop_still_flashes_after_a_build_up() {
        let (clock, now) = manual_clock();
        let source = AudioSource::new(clock);
        let loud = Levels { bass: 1.0, ..Levels::default() };
        let green = Color::rgb(0, 255, 0);
        let mut effect = music_drop(source.clone(), green, 0.4, 3.0, 0.0, 0.5).unwrap();
        let mut t = 0.0;
        source.publish(loud, 0.0);
        advance(&mut effect, &mut t, 20.0);
        source.publish(Levels::default(), 0.0);
        advance(&mut effect, &mut t, 8.0); // a lull
        source.publish(Levels { bass: 0.5, ..Levels::default() }, 0.0);
        advance(&mut effect, &mut t, 12.0); // building back up, no beats
        t += 0.05;
        set(&now, t);
        source.publish(loud, 10.0);
        assert_eq!(effect(t), green); // the lull is still remembered: this is the drop
    }

    #[test]
    fn drop_goes_dark_in_long_silence() {
        let (clock, _) = manual_clock();
        let source = AudioSource::new(clock);
        let mut effect = music_drop(source.clone(), Color::rgb(0, 255, 0), 0.4, 3.0, 0.0, 0.5).unwrap();
        let mut t = 0.0;
        source.publish(Levels { bass: 1.0, ..Levels::default() }, 0.0);
        advance(&mut effect, &mut t, 20.0);
        source.publish(Levels::default(), 0.0);
        assert_eq!(advance(&mut effect, &mut t, 120.0), OFF);
    }

    #[test]
    fn drop_refuses_what_does_not_fit() {
        let (clock, _) = manual_clock();
        let source = AudioSource::new(clock);
        for (flash, build, delay, sensitivity) in
            [(0.0, 3.0, 0.0, 0.5), (0.4, 0.0, 0.0, 0.5), (0.4, 3.0, 5.0, 0.5), (0.4, 3.0, 0.0, 2.0)]
        {
            assert!(music_drop(source.clone(), WHITE, flash, build, delay, sensitivity).is_err());
        }
    }

    #[test]
    fn ambient_breathes_and_never_goes_dark() {
        let (clock, now) = manual_clock();
        let source = AudioSource::new(clock);
        let mut effect = music_ambient(source.clone(), Color::rgb(200, 0, 0), BLUE, 6.0, 5.0, 0.0, 0.5).unwrap();
        assert_eq!(effect(0.0), Color::rgb(50, 0, 0)); // never beat and silent: 200 * 0.25
        assert_eq!(effect(3.0), Color::rgb(100, 0, 0)); // half way through the swell
        set(&now, 10.0);
        source.publish(Levels::default(), 10.0);
        assert_eq!(effect(3.0), BLUE); // a beat shows the accent
        set(&now, 20.0);
        assert_eq!(effect(3.0), Color::rgb(100, 0, 0)); // and the breathing is back
    }

    #[test]
    fn ambient_refuses_what_does_not_fit() {
        let (clock, _) = manual_clock();
        let source = AudioSource::new(clock);
        for (period, decay, delay, sensitivity) in
            [(0.0, 5.0, 0.0, 0.5), (6.0, 0.0, 0.0, 0.5), (6.0, 5.0, 5.0, 0.5), (6.0, 5.0, 0.0, 2.0)]
        {
            assert!(music_ambient(source.clone(), WARM, WHITE, period, decay, delay, sensitivity).is_err());
        }
    }

    #[test]
    fn bands_show_the_bass_and_the_treble_in_their_own_colours() {
        let (clock, _) = manual_clock();
        let source = AudioSource::new(clock);
        let mut effect = music_bands(source.clone(), RED, BLUE, 2.0, 0.0).unwrap();
        assert_eq!(effect(0.0), OFF);
        source.publish(Levels { bass: 1.0, ..Levels::default() }, 0.0);
        assert_eq!(effect(0.1), RED);
        source.publish(Levels { treble: 1.0, mid: 1.0, ..Levels::default() }, 0.0);
        assert_eq!(effect(0.35), Color::rgb(64, 0, 255)); // the bass has fallen to a half, squared
        source.publish(Levels::default(), 0.0);
        assert_eq!(effect(5.0), OFF);
        assert!(music_bands(source, RED, BLUE, 0.0, 0.0).is_err());
    }

    #[test]
    fn energy_picks_the_colour_by_how_loud_it_has_been() {
        let (clock, _) = manual_clock();
        let source = AudioSource::new(clock);
        let mut effect = music_energy(source.clone(), &[BLUE, RED], 2.0, 0.0, 0.0).unwrap();
        assert_eq!(effect(0.0), OFF);
        source.publish(Levels { mid: 1.0, ..Levels::default() }, 0.0);
        assert_eq!(effect(0.05), BLUE); // loud just now, calm until now
        let later = effect(1.0);
        assert!(later.r > 100 && later.b > 30, "{later:?}"); // on its way
        assert_eq!(effect(30.0), RED);
        let mut lit = music_energy(source.clone(), &[BLUE, RED], 2.0, 0.5, 0.0).unwrap();
        source.publish(Levels::default(), 0.0);
        assert_eq!(lit(0.0), Color::rgb(0, 0, 128)); // the floor, in the calm colour
        assert!(music_energy(source.clone(), &[], 2.0, 0.0, 0.0).is_err());
        assert!(music_energy(source, &[BLUE], 0.0, 0.0, 0.0).is_err());
    }

    #[test]
    fn chill_swells_slowly_and_keeps_its_floor() {
        let (clock, _) = manual_clock();
        let source = AudioSource::new(clock);
        let mut effect = music_chill(source.clone(), 60.0, 1.0, 0.2, 1.0, 0.0).unwrap();
        assert_eq!(effect(0.0), Color::rgb(51, 0, 0)); // silent: the floor, at the start of the wheel
        source.publish(Levels { bass: 1.0, ..Levels::default() }, 0.0);
        let soon = effect(0.1).r;
        assert!((60..=80).contains(&soon), "{soon}"); // a loud moment does not flash
        assert!(effect(10.0).r.max(effect(10.0).g) >= 250);
        source.publish(Levels::default(), 0.0);
        let sinking = effect(10.5);
        assert!(sinking.r.max(sinking.g) > 150, "{sinking:?}"); // nor does it drop at once
        for (period, saturation, floor, smoothing) in
            [(0.0, 1.0, 0.2, 1.0), (60.0, 2.0, 0.2, 1.0), (60.0, 1.0, 2.0, 1.0), (60.0, 1.0, 0.2, -1.0)]
        {
            assert!(music_chill(source.clone(), period, saturation, floor, smoothing, 0.0).is_err());
        }
    }
}
