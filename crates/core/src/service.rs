//! The background service: keeps the connection, the running effect and the remembered state.
//!
//! A port of `chsmartbulb.service.BulbService`. Requests and replies are the same JSON
//! objects as the Python service's socket protocol, so the web interface and the
//! agents talk to either one without knowing which.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use tokio::sync::{watch, Mutex, Notify};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::audio::{self, AudioSource, Clock, Levels};
use crate::bulb::Bulb;
use crate::catalog::{self, Needs, Sources};
use crate::color::{parse_color, Color, WHITE};
use crate::effects::Effect;
use crate::error::{invalid, Error, Result};
use crate::protocol::{self as p, frame_type, Frame, NativeEffect};
use crate::screen::ScreenSource;

const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);

/// Something on this device that can hear the music, started while an effect needs it.
#[async_trait]
pub trait AudioCapture: Send + Sync {
    /// Begin publishing into `source`; fails when there is nothing to capture with.
    async fn start(&self, source: Arc<AudioSource>) -> Result<()>;
    async fn stop(&self);
}

/// Something on this device that can see a screen, started while an effect needs it.
#[async_trait]
pub trait ScreenCapture: Send + Sync {
    async fn start(&self, source: Arc<ScreenSource>) -> Result<()>;
    async fn stop(&self);
}

/// What the light should be showing; survives reconnects and restarts.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub on: bool,
    pub color: Color,
    pub brightness: f64,
    /// `{"name": ..., "params": {...}}`
    pub effect: Option<Value>,
    /// `{"name": ..., "speed": ...}`
    pub native: Option<Value>,
}

impl Default for Plan {
    fn default() -> Self {
        Self { on: true, color: WHITE, brightness: 1.0, effect: None, native: None }
    }
}

impl Plan {
    pub fn to_json(&self) -> Value {
        json!({
            "on": self.on,
            "color": self.color.to_hex(),
            "brightness": self.brightness,
            "effect": self.effect,
            "native": self.native,
        })
    }

    pub fn from_json(data: &Value) -> Result<Self> {
        let field = |key: &str| data.get(key).ok_or_else(|| invalid(format!("missing field '{key}'")));
        let optional = |key: &str| data.get(key).filter(|v| !v.is_null()).cloned();
        Ok(Self {
            on: field("on")?.as_bool().ok_or_else(|| invalid("on must be true or false"))?,
            color: parse_color(field("color")?.as_str().ok_or_else(|| invalid("color must be text"))?)?,
            brightness: field("brightness")?.as_f64().ok_or_else(|| invalid("brightness must be a number"))?,
            effect: optional("effect"),
            native: optional("native"),
        })
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub state_path: Option<PathBuf>,
    pub fps: f64,
    pub retry_delay: Duration,
    pub poll_interval: Duration,
    pub save_delay: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            state_path: None,
            fps: crate::effects::DEFAULT_FPS,
            retry_delay: Duration::from_secs(5),
            poll_interval: Duration::from_secs(2),
            save_delay: Duration::from_secs(1),
        }
    }
}

/// What an effect follows: an agent's stream while one is connected, this device otherwise.
#[derive(Default)]
struct Feeds {
    audio_agents: usize,
    screen_agents: usize,
    /// The local capture currently running for an effect, to stop when it ends.
    audio_running: bool,
    screen_running: bool,
}

struct Inner {
    bulb: Arc<Bulb>,
    options: Options,
    clock: Clock,
    plan: StdMutex<Plan>,
    /// No remembered state yet: take over what the bulb shows.
    adopt_on_connect: AtomicBool,
    effect_task: StdMutex<Option<JoinHandle<()>>>,
    /// One request changes the plan at a time.
    lock: Mutex<()>,
    supervisor: StdMutex<Option<JoinHandle<()>>>,
    connecting: AtomicBool,
    problem: StdMutex<Option<String>>,
    retry_now: Notify,
    state: watch::Sender<Value>,
    watchers: AtomicUsize,
    feeds: StdMutex<Feeds>,
    remote_audio: Arc<AudioSource>,
    remote_screen: Arc<ScreenSource>,
    local_audio: Option<Arc<dyn AudioCapture>>,
    local_screen: Option<Arc<dyn ScreenCapture>>,
    pending_save: StdMutex<Option<JoinHandle<()>>>,
}

fn locked<T>(mutex: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A cheap handle to the service; clones share it.
#[derive(Clone)]
pub struct Service {
    inner: Arc<Inner>,
}

/// Keeps a client counted among the watchers; dropping it stops that.
pub struct Subscription {
    inner: Weak<Inner>,
    pub updates: watch::Receiver<Value>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            inner.watchers.fetch_sub(1, Ordering::SeqCst);
            inner.notify();
        }
    }
}

pub struct Builder {
    bulb: Arc<Bulb>,
    options: Options,
    clock: Clock,
    local_audio: Option<Arc<dyn AudioCapture>>,
    local_screen: Option<Arc<dyn ScreenCapture>>,
}

impl Builder {
    pub fn options(mut self, options: Options) -> Self {
        self.options = options;
        self
    }

    pub fn clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    pub fn audio_capture(mut self, capture: Arc<dyn AudioCapture>) -> Self {
        self.local_audio = Some(capture);
        self
    }

    pub fn screen_capture(mut self, capture: Arc<dyn ScreenCapture>) -> Self {
        self.local_screen = Some(capture);
        self
    }

    pub fn build(self) -> Service {
        let (state, _) = watch::channel(Value::Null);
        let inner = Arc::new(Inner {
            bulb: self.bulb,
            options: self.options,
            remote_audio: AudioSource::new(self.clock.clone()),
            clock: self.clock,
            plan: StdMutex::new(Plan::default()),
            adopt_on_connect: AtomicBool::new(true),
            effect_task: StdMutex::new(None),
            lock: Mutex::new(()),
            supervisor: StdMutex::new(None),
            connecting: AtomicBool::new(false),
            problem: StdMutex::new(None),
            retry_now: Notify::new(),
            state,
            watchers: AtomicUsize::new(0),
            feeds: StdMutex::new(Feeds::default()),
            remote_screen: ScreenSource::new(),
            local_audio: self.local_audio,
            local_screen: self.local_screen,
            pending_save: StdMutex::new(None),
        });
        inner.state.send_replace(inner.state_json());
        Service { inner }
    }
}

impl Service {
    pub fn builder(bulb: Arc<Bulb>) -> Builder {
        Builder { bulb, options: Options::default(), clock: audio::monotonic(), local_audio: None, local_screen: None }
    }

    pub fn bulb(&self) -> &Arc<Bulb> {
        &self.inner.bulb
    }

    /// Connect once and take over the bulb's current state (no supervision).
    pub async fn attach(&self) -> Result<()> {
        self.inner.bulb.connect().await?;
        self.inner.adopt();
        self.inner.notify();
        Ok(())
    }

    /// Load the remembered state and keep the bulb connected in the background.
    pub fn start(&self) {
        self.inner.load();
        let inner = self.inner.clone();
        let task = tokio::spawn(async move { inner.supervise().await });
        if let Some(old) = locked(&self.inner.supervisor).replace(task) {
            old.abort();
        }
    }

    /// Stop supervising, stop the effect, let go of the bulb and save the state.
    pub async fn close(&self) {
        let supervisor = locked(&self.inner.supervisor).take();
        if let Some(task) = supervisor {
            task.abort();
            let _ = task.await;
        }
        self.inner.stop_effect().await;
        self.inner.bulb.disconnect().await;
        self.inner.write_state();
        self.inner.notify();
    }

    /// Wait until the running effect ends by itself.
    pub async fn wait_effect(&self) {
        loop {
            if !self.inner.playing() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub fn state(&self) -> Value {
        self.inner.state_json()
    }

    pub fn plan(&self) -> Plan {
        locked(&self.inner.plan).clone()
    }

    /// Follow the state; the first value is the current one.
    pub fn subscribe(&self) -> Subscription {
        self.inner.watchers.fetch_add(1, Ordering::SeqCst);
        self.inner.notify(); // one more is watching
        let mut updates = self.inner.state.subscribe();
        updates.mark_unchanged();
        Subscription { inner: Arc::downgrade(&self.inner), updates }
    }

    /// Run one request and return `{"ok": true, ...}` or `{"ok": false, "error": ...}`.
    pub async fn handle(&self, request: &Value) -> Value {
        let inner = &self.inner;
        let reply = match request.get("cmd").and_then(Value::as_str) {
            Some(command) if COMMANDS.contains(&command) => {
                let result = {
                    let _guard = inner.lock.lock().await;
                    inner.run(command, request).await
                };
                match result {
                    Ok(Value::Object(mut fields)) => {
                        let mut reply = Map::new();
                        reply.insert("ok".into(), Value::Bool(true));
                        reply.append(&mut fields);
                        Value::Object(reply)
                    }
                    Ok(_) => json!({"ok": true}),
                    Err(error) => json!({"ok": false, "error": error.to_string()}),
                }
            }
            _ => {
                let shown = match request.get("cmd") {
                    None | Some(Value::Null) => "None".to_string(),
                    Some(Value::String(name)) => format!("'{name}'"),
                    Some(other) => other.to_string(),
                };
                json!({"ok": false, "error": format!("unknown command {shown}")})
            }
        };
        inner.notify();
        reply
    }

    /// An agent of `kind` came in: effects that follow it switch to its feed.
    pub async fn agent_joined(&self, kind: Needs) {
        {
            let _guard = self.inner.lock.lock().await;
            {
                let mut feeds = locked(&self.inner.feeds);
                match kind {
                    Needs::Audio => feeds.audio_agents += 1,
                    Needs::Screen => feeds.screen_agents += 1,
                }
            }
            log::info!("{} agent connected", kind.as_str());
            self.inner.feed_changed(kind).await;
        }
        self.inner.notify();
    }

    pub async fn agent_left(&self, kind: Needs) {
        {
            let _guard = self.inner.lock.lock().await;
            let gone = {
                let mut feeds = locked(&self.inner.feeds);
                let agents = match kind {
                    Needs::Audio => &mut feeds.audio_agents,
                    Needs::Screen => &mut feeds.screen_agents,
                };
                *agents = agents.saturating_sub(1);
                *agents == 0
            };
            log::info!("{} agent disconnected", kind.as_str());
            if gone {
                match kind {
                    Needs::Audio => self.inner.remote_audio.clear(),
                    Needs::Screen => self.inner.remote_screen.clear(),
                }
                self.inner.feed_changed(kind).await;
            }
        }
        self.inner.notify();
    }

    /// One block of an agent's feed. Malformed blocks are dropped: not worth a reply at forty a second.
    pub fn push(&self, kind: Needs, request: &Value) {
        match kind {
            Needs::Audio => {
                let Some(levels) = request.get("levels").and_then(Value::as_array) else { return };
                let values: Vec<f64> = levels.iter().filter_map(Value::as_f64).map(|v| v.clamp(0.0, 1.0)).collect();
                let [bass, mid, treble] = values[..] else { return };
                let balance = request.get("balance").and_then(Value::as_f64).unwrap_or(0.0).clamp(-1.0, 1.0);
                // agents from before the sensitivity setting decided on the beat themselves
                let onset = match request.get("onset") {
                    Some(value) => match value.as_f64() {
                        Some(onset) => onset,
                        None => return,
                    },
                    None if request.get("beat").and_then(Value::as_bool).unwrap_or(false) => f64::INFINITY,
                    None => 0.0,
                };
                self.inner.remote_audio.publish(Levels { bass, mid, treble, balance }, onset);
            }
            Needs::Screen => {
                if let Some(Ok(color)) = request.get("color").and_then(Value::as_str).map(parse_color) {
                    self.inner.remote_screen.push(color);
                }
            }
        }
    }

    /// The source a local capture publishes into while no agent feeds this kind, for tests and platform layers.
    pub fn remote_audio(&self) -> Arc<AudioSource> {
        self.inner.remote_audio.clone()
    }
}

const COMMANDS: [&str; 14] = [
    "status",
    "on",
    "off",
    "color",
    "brightness",
    "effect",
    "native",
    "stop",
    "reconnect",
    "effects",
    "info",
    "timers",
    "timer",
    "raw",
];

fn field<'a>(request: &'a Value, key: &str) -> Result<&'a Value> {
    request.get(key).ok_or_else(|| invalid(format!("missing field '{key}'")))
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn as_number(value: &Value, what: &str) -> Result<f64> {
    match value {
        Value::Number(n) => n.as_f64().ok_or_else(|| invalid(format!("{what} must be a number"))),
        Value::String(s) => s.trim().parse().map_err(|_| invalid(format!("could not convert string to float: {s:?}"))),
        Value::Bool(b) => Ok(f64::from(u8::from(*b))),
        other => Err(invalid(format!("{what} must be a number, not {other}"))),
    }
}

fn as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn timer_json(timer: &p::Timer) -> Value {
    json!({
        "index": timer.index,
        "name": timer.name,
        "enabled": timer.enabled,
        "days": timer.days,
        "hour": timer.hour,
        "minute": timer.minute,
    })
}

fn needs_of(effect: &Value) -> Option<Needs> {
    effect.get("name").and_then(Value::as_str).and_then(catalog::lookup).and_then(|info| info.needs)
}

impl Inner {
    // --- state ------------------------------------------------------------------------

    fn state_json(&self) -> Value {
        let connected = self.bulb.is_connected();
        let link = if connected {
            "connected"
        } else if self.connecting.load(Ordering::SeqCst) {
            "connecting"
        } else {
            "waiting"
        };
        let (audio_agents, screen_agents) = {
            let feeds = locked(&self.feeds);
            (feeds.audio_agents, feeds.screen_agents)
        };
        let source = |agents: usize| if agents > 0 { "agent" } else { "local" };
        let mut state = json!({
            "connected": connected,
            "link": link,
            "problem": if connected { None } else { locked(&self.problem).clone() },
            "playing": self.playing(),
            "audio": source(audio_agents),
            "screen": source(screen_agents),
            "agents": {"audio": audio_agents, "screen": screen_agents},
            "watchers": self.watchers.load(Ordering::SeqCst),
        });
        if let (Value::Object(state), Value::Object(plan)) = (&mut state, locked(&self.plan).to_json()) {
            state.extend(plan);
        }
        state
    }

    fn notify(&self) {
        let state = self.state_json();
        self.state.send_if_modified(|told| {
            if *told == state {
                false
            } else {
                *told = state;
                true
            }
        });
    }

    fn playing(&self) -> bool {
        locked(&self.effect_task).as_ref().is_some_and(|task| !task.is_finished())
    }

    fn adopt(&self) {
        *locked(&self.plan) = Plan {
            on: self.bulb.is_on(),
            color: self.bulb.color(),
            brightness: self.bulb.brightness(),
            effect: None,
            native: None,
        };
        self.adopt_on_connect.store(false, Ordering::SeqCst);
    }

    fn load(&self) {
        let Some(path) = &self.options.state_path else { return };
        let Ok(text) = std::fs::read_to_string(path) else { return };
        let plan = serde_json::from_str::<Value>(&text)
            .map_err(|error| invalid(error.to_string()))
            .and_then(|data| Plan::from_json(&data))
            .and_then(|plan| {
                if let Some(effect) = &plan.effect {
                    let name = effect.get("name").and_then(Value::as_str).unwrap_or_default();
                    check_effect(name, effect.get("params").and_then(Value::as_object))?;
                }
                Ok(plan)
            });
        match plan {
            Ok(plan) => {
                *locked(&self.plan) = plan;
                self.adopt_on_connect.store(false, Ordering::SeqCst);
            }
            Err(error) => log::warn!("ignoring unusable state file {}: {error}", path.display()),
        }
    }

    /// Write the plan soon. Changes that follow closely, as from a dragged slider, share one write.
    fn save(self: &Arc<Self>) {
        if self.options.state_path.is_none() {
            return;
        }
        let mut pending = locked(&self.pending_save);
        if pending.as_ref().is_some_and(|task| !task.is_finished()) {
            return;
        }
        let weak = Arc::downgrade(self);
        let delay = self.options.save_delay;
        *pending = Some(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            if let Some(inner) = weak.upgrade() {
                inner.write_plan();
            }
        }));
    }

    fn write_state(&self) {
        let pending = locked(&self.pending_save).take();
        if let Some(task) = pending {
            task.abort();
            self.write_plan();
        }
    }

    fn write_plan(&self) {
        let Some(path) = &self.options.state_path else { return };
        let text = serde_json::to_string_pretty(&locked(&self.plan).to_json()).expect("plain JSON") + "\n";
        let written = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(path, text));
        if let Err(error) = written {
            log::warn!("cannot save state to {}: {error}", path.display());
        }
    }

    // --- connection -------------------------------------------------------------------

    async fn supervise(self: Arc<Self>) {
        let mut delay = self.options.retry_delay;
        loop {
            self.notify(); // a link that dropped by itself shows up here
            if self.bulb.is_connected() {
                tokio::time::sleep(self.options.poll_interval).await;
                continue;
            }
            self.connecting.store(true, Ordering::SeqCst);
            self.notify();
            let attempt = self.bulb.connect().await;
            self.connecting.store(false, Ordering::SeqCst);
            if let Err(error) = attempt {
                log::info!("bulb unavailable: {error}");
                *locked(&self.problem) = Some(error.to_string());
                let retry = self.retry_now.notified();
                tokio::pin!(retry);
                retry.as_mut().enable();
                self.notify();
                tokio::select! {
                    () = &mut retry => delay = self.options.retry_delay, // someone asked: they expect the bulb back
                    () = tokio::time::sleep(delay) => delay = (delay * 2).min(MAX_RETRY_DELAY),
                }
                continue;
            }
            log::info!("connected to {}", self.bulb.describe());
            *locked(&self.problem) = None;
            delay = self.options.retry_delay;
            if self.adopt_on_connect.load(Ordering::SeqCst) {
                self.adopt();
                self.save();
            } else if let Err(error) = self.apply(false, None).await {
                log::warn!("cannot restore the light: {error}");
            }
        }
    }

    /// Make the bulb show the plan. Does nothing while disconnected; the supervisor retries.
    async fn apply(self: &Arc<Self>, fade: bool, duration: Option<f64>) -> Result<()> {
        self.stop_effect().await;
        if !self.bulb.is_connected() {
            return Ok(());
        }
        let plan = locked(&self.plan).clone();
        let shown = async {
            if self.bulb.brightness() != plan.brightness {
                self.bulb.set_brightness(plan.brightness).await?;
            }
            if !plan.on {
                self.bulb.turn_off(fade).await
            } else if let Some(effect) = &plan.effect {
                self.start_effect(effect, duration).await
            } else if let Some(native) = &plan.native {
                let name = native.get("name").and_then(Value::as_str).unwrap_or_default();
                let effect =
                    NativeEffect::from_name(name).ok_or_else(|| invalid(format!("unknown native effect {name:?}")))?;
                let speed = native.get("speed").and_then(Value::as_u64).unwrap_or(8).min(255) as u8;
                self.bulb.set_native_effect(effect, Some(plan.color), speed).await
            } else {
                self.bulb.set_color(plan.color, fade).await
            }
        };
        match shown.await {
            Err(error) if error.is_link_error() => {
                self.link_lost(&error).await;
                Ok(())
            }
            other => other,
        }
    }

    async fn link_lost(&self, error: &Error) {
        log::info!("lost the bulb: {error}");
        self.bulb.disconnect().await;
        self.notify();
    }

    async fn commit(self: &Arc<Self>, fade: bool, duration: Option<f64>) -> Result<()> {
        self.adopt_on_connect.store(false, Ordering::SeqCst); // a request made while the bulb is away wins
        self.save();
        self.apply(fade, duration).await
    }

    // --- effects ----------------------------------------------------------------------

    async fn start_effect(self: &Arc<Self>, spec: &Value, duration: Option<f64>) -> Result<()> {
        let name = spec.get("name").and_then(Value::as_str).unwrap_or_default();
        let params = spec.get("params").and_then(Value::as_object);
        let mut sources = Sources::default();
        match needs_of(spec) {
            Some(Needs::Audio) => sources.audio = Some(self.audio_source().await),
            Some(Needs::Screen) => sources.screen = Some(self.screen_source().await),
            None => {}
        }
        let effect = catalog::create(name, params, &sources)?;
        let inner = self.clone();
        let task = tokio::spawn(async move { inner.run_effect(effect, duration).await });
        *locked(&self.effect_task) = Some(task);
        let watcher = Arc::downgrade(self);
        let fps = self.options.fps.min(self.bulb.bearer().fps());
        tokio::spawn(async move {
            // tell the clients once the effect ends on its own
            let interval = Duration::from_secs_f64(1.0 / fps.max(1.0));
            while let Some(inner) = watcher.upgrade() {
                if !inner.playing() {
                    inner.notify();
                    return;
                }
                drop(inner);
                tokio::time::sleep(interval).await;
            }
        });
        Ok(())
    }

    async fn audio_source(&self) -> Arc<AudioSource> {
        if locked(&self.feeds).audio_agents > 0 {
            return self.remote_audio.clone();
        }
        let Some(capture) = &self.local_audio else {
            log::warn!("nothing to capture sound with here: staying dark until an agent brings a feed");
            return self.remote_audio.clone();
        };
        let source = AudioSource::new(self.clock.clone());
        match capture.start(source.clone()).await {
            Ok(()) => {
                locked(&self.feeds).audio_running = true;
                source
            }
            Err(error) => {
                log::warn!("sound effect has no input: {error}");
                self.remote_audio.clone()
            }
        }
    }

    async fn screen_source(&self) -> Arc<ScreenSource> {
        if locked(&self.feeds).screen_agents > 0 {
            return self.remote_screen.clone();
        }
        let Some(capture) = &self.local_screen else { return self.remote_screen.clone() };
        let source = ScreenSource::new();
        match capture.start(source.clone()).await {
            Ok(()) => {
                locked(&self.feeds).screen_running = true;
                source
            }
            Err(error) => {
                log::warn!("screen effect has no input: {error}");
                self.remote_screen.clone()
            }
        }
    }

    async fn release_sources(&self) {
        let (audio, screen) = {
            let mut feeds = locked(&self.feeds);
            (std::mem::take(&mut feeds.audio_running), std::mem::take(&mut feeds.screen_running))
        };
        if audio {
            if let Some(capture) = &self.local_audio {
                capture.stop().await;
            }
        }
        if screen {
            if let Some(capture) = &self.local_screen {
                capture.stop().await;
            }
        }
    }

    async fn run_effect(self: Arc<Self>, effect: Effect, duration: Option<f64>) {
        let fps = self.options.fps;
        if let Err(error) = play(&self.bulb, effect, duration, fps).await {
            if error.is_link_error() {
                self.link_lost(&error).await; // the plan keeps the effect, so it resumes after reconnecting
            } else {
                log::warn!("effect stopped: {error}");
            }
            return;
        }
        // ran its full duration: go back to the plain colour
        let color = {
            let mut plan = locked(&self.plan);
            plan.effect = None;
            plan.color
        };
        self.save();
        self.release_sources().await;
        if let Err(error) = self.bulb.set_color(color, false).await {
            if error.is_link_error() {
                self.link_lost(&error).await;
            }
        }
    }

    async fn stop_effect(&self) {
        let task = locked(&self.effect_task).take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        self.release_sources().await;
    }

    async fn feed_changed(self: &Arc<Self>, kind: Needs) {
        let effect = locked(&self.plan).effect.clone();
        if self.playing() && effect.as_ref().and_then(needs_of) == Some(kind) {
            if let Err(error) = self.apply(false, None).await {
                log::warn!("cannot restart the effect on the other source: {error}");
            }
        }
    }

    // --- requests ---------------------------------------------------------------------

    fn require_link(&self) -> Result<()> {
        if self.bulb.is_connected() {
            Ok(())
        } else {
            Err(Error::NotConnected("the bulb is not connected".into()))
        }
    }

    fn take_brightness(&self, level: Option<&Value>) -> Result<()> {
        if let Some(level) = level.filter(|v| !v.is_null()) {
            let level = as_number(level, "brightness")?;
            if !(0.0..=1.0).contains(&level) {
                return Err(invalid("brightness must be within 0..1"));
            }
            locked(&self.plan).brightness = level;
        }
        Ok(())
    }

    async fn run(self: &Arc<Self>, command: &str, request: &Value) -> Result<Value> {
        let fade = truthy(request.get("fade"));
        match command {
            "status" => {
                let mut reply = self.state_json();
                if self.bulb.is_connected() {
                    match self.bulb.light_state().await {
                        Ok(state) => reply["bulb"] = json!(state.color.to_hex()),
                        Err(error) => log::debug!("status read-back failed: {error}"),
                    }
                }
                Ok(reply)
            }
            "on" | "off" => {
                locked(&self.plan).on = command == "on";
                self.commit(fade, None).await?;
                Ok(json!({}))
            }
            "color" => {
                let color = parse_color(&as_text(field(request, "color")?))?;
                self.take_brightness(request.get("brightness"))?;
                {
                    let mut plan = locked(&self.plan);
                    plan.effect = None;
                    plan.native = None;
                    plan.on = !color.is_off();
                    if plan.on {
                        plan.color = color;
                    }
                }
                self.commit(fade, None).await?;
                Ok(json!({}))
            }
            "brightness" => {
                self.take_brightness(Some(field(request, "level")?))?;
                self.adopt_on_connect.store(false, Ordering::SeqCst);
                self.save();
                if self.playing() && self.bulb.is_connected() {
                    // a running effect picks the new level up on its next frame
                    let level = locked(&self.plan).brightness;
                    if let Err(error) = self.bulb.set_brightness(level).await {
                        if !error.is_link_error() {
                            return Err(error);
                        }
                        self.link_lost(&error).await;
                    }
                } else {
                    self.apply(false, None).await?;
                }
                Ok(json!({}))
            }
            "effect" => {
                let name = as_text(field(request, "name")?);
                let params = match request.get("params") {
                    None | Some(Value::Null) => Map::new(),
                    Some(Value::Object(params)) => params.clone(),
                    Some(other) => return Err(invalid(format!("params must be an object, got {other}"))),
                };
                check_effect(&name, Some(&params))?;
                self.take_brightness(request.get("brightness"))?;
                let duration = match request.get("duration") {
                    None | Some(Value::Null) => None,
                    Some(value) => Some(as_number(value, "duration")?),
                };
                {
                    let mut plan = locked(&self.plan);
                    plan.effect = Some(json!({"name": name, "params": params}));
                    plan.native = None;
                    plan.on = true;
                }
                self.commit(false, duration).await?;
                Ok(json!({}))
            }
            "native" => {
                let name = as_text(field(request, "name")?).to_lowercase();
                let effect = NativeEffect::from_name(&name)
                    .filter(|effect| *effect != NativeEffect::Fixed)
                    .ok_or_else(|| invalid(format!("unknown native effect {name:?}")))?;
                let speed = match request.get("speed") {
                    None | Some(Value::Null) => 8.0,
                    Some(value) => as_number(value, "speed")?.trunc(),
                };
                if !(0.0..=15.0).contains(&speed) {
                    return Err(invalid("speed level must be 0..15"));
                }
                p::speed_byte(effect, speed as u8)?;
                self.take_brightness(request.get("brightness"))?;
                let color = match request.get("color") {
                    Some(value) if truthy(Some(value)) => Some(parse_color(&as_text(value))?),
                    _ => None,
                };
                {
                    let mut plan = locked(&self.plan);
                    if let Some(color) = color {
                        plan.color = color;
                    }
                    plan.native = Some(json!({"name": name, "speed": speed as u8}));
                    plan.effect = None;
                    plan.on = true;
                }
                self.commit(false, None).await?;
                Ok(json!({}))
            }
            "stop" => {
                {
                    let mut plan = locked(&self.plan);
                    plan.effect = None;
                    plan.native = None;
                }
                self.commit(false, None).await?;
                Ok(json!({}))
            }
            "reconnect" => {
                self.retry_now.notify_waiters();
                Ok(json!({}))
            }
            "effects" => {
                let names: Vec<&str> =
                    NativeEffect::ALL.iter().filter(|e| **e != NativeEffect::Fixed).map(|e| e.name()).collect();
                Ok(json!({"effects": catalog::describe(), "native": {"names": names, "speed": [0, 15]}}))
            }
            "info" => {
                self.require_link()?;
                let info = self.bulb.info().await?;
                Ok(json!({
                    "name": info.name,
                    "version": info.version,
                    "model": info.model,
                    "model_id": info.model_id,
                    "device_id": info.device_id,
                }))
            }
            "timers" => {
                self.require_link()?;
                let timers: Vec<Value> = self.bulb.timers().await?.iter().map(timer_json).collect();
                Ok(json!({"timers": timers}))
            }
            "timer" => {
                self.require_link()?;
                let index = as_number(field(request, "index")?, "index")?;
                if !(0.0..=255.0).contains(&index) {
                    return Err(invalid(format!("the bulb has no timer with index {index}")));
                }
                let enabled = truthy(Some(field(request, "enabled")?));
                let timer = self.bulb.set_timer_enabled(index as u8, enabled).await?;
                Ok(json!({"timer": timer_json(&timer)}))
            }
            "raw" => {
                self.require_link()?;
                let frame = Frame::decode(&p::unhex(&as_text(field(request, "hex")?))?)?;
                if frame.kind == frame_type::QUERY {
                    let answer = self.bulb.request(frame).await?;
                    return Ok(json!({"answer": p::hex(&answer.encode()?)}));
                }
                self.bulb.send(&frame).await?;
                Ok(json!({}))
            }
            _ => unreachable!("checked against COMMANDS"),
        }
    }
}

/// Build the effect once, so a bad request fails before it reaches the plan.
fn check_effect(name: &str, params: Option<&Map<String, Value>>) -> Result<()> {
    catalog::resolve(name, params)?;
    let sources = Sources { audio: Some(AudioSource::new(audio::monotonic())), screen: Some(ScreenSource::new()) };
    catalog::create(name, params, &sources).map(drop)
}

/// Stream `effect` to the bulb until `duration` elapses (forever when `None`).
///
/// Frames are scheduled against the start time so slow sends do not accumulate
/// drift; over a slow bearer the loop simply sends the latest colour it can.
pub async fn play(bulb: &Bulb, mut effect: Effect, duration: Option<f64>, fps: f64) -> Result<()> {
    if fps <= 0.0 {
        return Err(invalid("fps must be positive"));
    }
    let interval = 1.0 / fps;
    let start = Instant::now();
    let mut last: Option<Color> = None;
    let mut frame: u64 = 0;
    loop {
        let elapsed = start.elapsed().as_secs_f64();
        if duration.is_some_and(|duration| elapsed >= duration) {
            return Ok(());
        }
        let color = effect(elapsed);
        if last != Some(color) {
            bulb.set_color(color, false).await?;
            last = Some(color);
        }
        frame += 1;
        let next = start + Duration::from_secs_f64(frame as f64 * interval);
        tokio::time::sleep_until(next.max(Instant::now())).await;
    }
}

/// One client of the service, whatever carries its messages: the rules of the socket protocol.
///
/// A client that is not trusted must start with `{"cmd": "auth", "token": ...}`.
/// Agents stream `audio` or `screen` blocks, which are not answered.
pub struct Session {
    service: Service,
    token: Option<String>,
    trusted: bool,
    feeding: HashSet<Needs>,
    subscription: Option<Subscription>,
}

/// What to do with one message of a [`Session`].
pub struct Outcome {
    pub reply: Option<Value>,
    /// The client was refused and the connection should end after the reply.
    pub close: bool,
}

impl Session {
    /// `token` is what the client must present first; `None` trusts it from the start.
    pub fn new(service: Service, token: Option<String>) -> Self {
        let trusted = token.is_none();
        Self { service, token, trusted, feeding: HashSet::new(), subscription: None }
    }

    /// State updates once the client has subscribed.
    pub fn updates(&mut self) -> Option<&mut watch::Receiver<Value>> {
        self.subscription.as_mut().map(|subscription| &mut subscription.updates)
    }

    pub async fn receive(&mut self, line: &str) -> Outcome {
        let request: Value = match serde_json::from_str::<Value>(line) {
            Ok(request @ Value::Object(_)) => request,
            Ok(_) => {
                return self.answer(None, json!({"ok": false, "error": "bad request: request must be a JSON object"}))
            }
            Err(error) => return self.answer(None, json!({"ok": false, "error": format!("bad request: {error}")})),
        };
        let command = request.get("cmd").and_then(Value::as_str).unwrap_or_default();
        let reply = if command == "auth" {
            let offered = request.get("token").map(as_text).unwrap_or_default();
            let expected = self.token.clone().unwrap_or_default();
            self.trusted = self.trusted || constant_time_eq(offered.as_bytes(), expected.as_bytes());
            if self.trusted {
                json!({"ok": true})
            } else {
                json!({"ok": false, "error": "wrong token"})
            }
        } else if !self.trusted {
            json!({"ok": false, "error": "not authorised"})
        } else if let Some(kind) = match command {
            "audio" => Some(Needs::Audio),
            "screen" => Some(Needs::Screen),
            _ => None,
        } {
            if self.feeding.insert(kind) {
                self.service.agent_joined(kind).await;
            }
            self.service.push(kind, &request);
            return Outcome { reply: None, close: false }; // an agent's stream is not acknowledged
        } else if command == "subscribe" {
            if self.subscription.is_none() {
                self.subscription = Some(self.service.subscribe());
            }
            let mut reply = json!({"ok": true});
            if let (Value::Object(reply), Value::Object(state)) = (&mut reply, self.service.state()) {
                reply.extend(state);
            }
            reply
        } else {
            self.service.handle(&request).await
        };
        self.answer(Some(&request), reply)
    }

    fn answer(&self, request: Option<&Value>, mut reply: Value) -> Outcome {
        if let Some(id) = request.and_then(|request| request.get("id")) {
            reply["id"] = id.clone(); // lets a client that also gets events match its replies
        }
        Outcome { reply: Some(reply), close: !self.trusted }
    }

    /// The client is gone: agents it stood for leave.
    pub async fn close(mut self) {
        self.subscription.take();
        for kind in std::mem::take(&mut self.feeding) {
            self.service.agent_left(kind).await;
        }
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}
