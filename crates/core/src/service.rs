//! The background service: keeps the connection, the running effect and the remembered state.
//!
//! Requests and replies are the JSON objects of the socket protocol (`docs/usage.md`),
//! which is all the web interface, the command line and the agents know of it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use tokio::sync::{mpsc, watch, Mutex, Notify};
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
    /// Whether a started capture still delivers; one that ended by itself is started again.
    fn alive(&self) -> bool {
        true
    }
}

/// Something on this device that can see a screen, started while an effect needs it.
#[async_trait]
pub trait ScreenCapture: Send + Sync {
    /// The monitors that can be watched, as `{"index", "width", "height"}` counted from 1;
    /// empty where the screen cannot be captured.
    fn monitors(&self) -> Vec<Value>;
    /// Begin pushing the colour of `monitor` into `source`; 0 is every monitor together.
    async fn start(&self, source: Arc<ScreenSource>, monitor: i64) -> Result<()>;
    async fn stop(&self);
    /// Whether the system has the user choose the screen itself, as a Wayland desktop does:
    /// there are no monitors to list then, only [`ScreenCapture::choose_again`].
    fn asks(&self) -> bool {
        false
    }
    /// Forget the chosen screen and stop watching it, so the next start asks for one.
    async fn choose_again(&self) {}
    /// Whether a started capture still delivers; one that ended by itself is started again.
    fn alive(&self) -> bool {
        true
    }
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

/// How bright a dimmed light stays while the computer is locked.
pub const AWAY_DIM: f64 = 0.1;
/// Why a computer can be away.
pub const AWAY_REASONS: [&str; 3] = ["lock", "sleep", "shutdown"];

/// What the light does while the computer is away for some reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AwayLook {
    /// Down to [`AWAY_DIM`], keeping the colour.
    Dim,
    Off,
}

/// What happens to the computer, for the light to follow it being away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Presence {
    Lock,
    Unlock,
    Sleep,
    Resume,
    Shutdown,
}

impl Presence {
    /// The service request that goes with it, as `chsmartbulb.presence.request_of` builds it.
    pub fn request(self) -> Value {
        match self {
            Presence::Unlock | Presence::Resume => json!({"cmd": "back"}),
            Presence::Lock => json!({"cmd": "away", "reason": "lock"}),
            Presence::Sleep => json!({"cmd": "away", "reason": "sleep"}),
            Presence::Shutdown => json!({"cmd": "away", "reason": "shutdown"}),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub state_path: Option<PathBuf>,
    pub fps: f64,
    pub retry_delay: Duration,
    pub poll_interval: Duration,
    pub save_delay: Duration,
    /// The look for each reason the computer is away; a reason without one is ignored.
    pub away_looks: HashMap<String, AwayLook>,
    /// Which of this device's monitors the screen effect follows, until a request chooses another.
    pub monitor: i64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            state_path: None,
            fps: crate::effects::DEFAULT_FPS,
            retry_delay: Duration::from_secs(5),
            poll_interval: Duration::from_secs(2),
            save_delay: Duration::from_secs(1),
            away_looks: HashMap::new(),
            monitor: crate::screen::PRIMARY,
        }
    }
}

/// What an effect follows: an agent's stream while one is connected, this device otherwise.
#[derive(Default)]
struct Feeds {
    audio_agents: usize,
    screen_agents: usize,
    /// What the local capture running for an effect publishes into, to stop when no effect needs it.
    audio_local: Option<Arc<AudioSource>>,
    /// With the monitor it watches.
    screen_local: Option<(Arc<ScreenSource>, i64)>,
}

/// The local captures of an effect being replaced, kept up for the next one: reopening
/// a capture disturbs what the computer is playing.
#[derive(Default)]
struct Held {
    audio: Option<Arc<AudioSource>>,
    screen: Option<(Arc<ScreenSource>, i64)>,
}

/// Another machine feeding its sound or screen, as far as it said who it is.
struct Agent {
    id: String,
    kind: Needs,
    name: Option<String>,
    /// `{"index", "width", "height"}` for each monitor of a screen agent.
    monitors: Vec<Value>,
    monitor: Option<i64>,
    /// What the service tells this agent.
    events: mpsc::UnboundedSender<Value>,
}

impl Agent {
    fn info(&self) -> Value {
        json!({"id": self.id, "name": self.name, "monitors": self.monitors, "monitor": self.monitor})
    }

    fn has_monitor(&self, index: i64) -> bool {
        index == 0 || self.monitors.iter().any(|area| area["index"] == index) // 0 is every monitor together
    }
}

#[derive(Default)]
struct Agents {
    counter: usize,
    list: Vec<Agent>,
    /// The monitor an agent of that kind and name last chose.
    chosen: HashMap<(Needs, String), i64>,
}

struct Inner {
    bulb: Arc<Bulb>,
    options: Options,
    clock: Clock,
    plan: StdMutex<Plan>,
    /// No remembered state yet: take over what the bulb shows.
    adopt_on_connect: AtomicBool,
    effect_task: StdMutex<Option<JoinHandle<()>>>,
    /// An effect is playing; cleared when it is stopped or ends by itself.
    effect_running: AtomicBool,
    /// One request changes the plan at a time.
    lock: Mutex<()>,
    supervisor: StdMutex<Option<JoinHandle<()>>>,
    connecting: AtomicBool,
    problem: StdMutex<Option<String>>,
    retry_now: Notify,
    state: watch::Sender<Value>,
    watchers: AtomicUsize,
    feeds: StdMutex<Feeds>,
    agents: StdMutex<Agents>,
    remote_audio: Arc<AudioSource>,
    remote_screen: Arc<ScreenSource>,
    local_audio: Option<Arc<dyn AudioCapture>>,
    local_screen: Option<Arc<dyn ScreenCapture>>,
    /// What the local screen capture offers; empty when there is none or it cannot capture.
    local_monitors: Vec<Value>,
    local_monitor: AtomicI64,
    pending_save: StdMutex<Option<JoinHandle<()>>>,
    away_looks: StdMutex<HashMap<String, AwayLook>>,
    /// Why the computer is away, while it is: the light shows the away look, the plan stays as it is.
    away: StdMutex<Option<String>>,
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
        let away_looks = self.options.away_looks.clone();
        let local_monitors = self.local_screen.as_ref().map(|capture| capture.monitors()).unwrap_or_default();
        let local_monitor = self.options.monitor;
        let inner = Arc::new(Inner {
            bulb: self.bulb,
            options: self.options,
            remote_audio: AudioSource::new(self.clock.clone()),
            clock: self.clock,
            plan: StdMutex::new(Plan::default()),
            adopt_on_connect: AtomicBool::new(true),
            effect_task: StdMutex::new(None),
            effect_running: AtomicBool::new(false),
            lock: Mutex::new(()),
            supervisor: StdMutex::new(None),
            connecting: AtomicBool::new(false),
            problem: StdMutex::new(None),
            retry_now: Notify::new(),
            state,
            watchers: AtomicUsize::new(0),
            feeds: StdMutex::new(Feeds::default()),
            agents: StdMutex::new(Agents::default()),
            remote_screen: ScreenSource::new(),
            local_audio: self.local_audio,
            local_screen: self.local_screen,
            local_monitors,
            local_monitor: AtomicI64::new(local_monitor),
            pending_save: StdMutex::new(None),
            away_looks: StdMutex::new(away_looks),
            away: StdMutex::new(None),
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

    /// An agent of `kind` came in: effects that follow it switch to its feed. `events` is how the
    /// service reaches it; the result names it for [`Service::agent_hello`] and [`Service::agent_left`].
    pub async fn agent_joined(&self, kind: Needs, events: mpsc::UnboundedSender<Value>) -> String {
        let id = {
            let _guard = self.inner.lock.lock().await;
            let id = {
                let mut agents = locked(&self.inner.agents);
                agents.counter += 1;
                let id = format!("{}-{}", kind.as_str(), agents.counter);
                agents.list.push(Agent {
                    id: id.clone(),
                    kind,
                    name: None,
                    monitors: Vec::new(),
                    monitor: None,
                    events,
                });
                id
            };
            {
                let mut feeds = locked(&self.inner.feeds);
                match kind {
                    Needs::Audio => feeds.audio_agents += 1,
                    Needs::Screen => feeds.screen_agents += 1,
                }
            }
            log::info!("{} agent connected", kind.as_str());
            self.inner.feed_changed(kind).await;
            id
        };
        self.inner.notify();
        id
    }

    /// An agent says who it is; one that chose a monitor before gets that one back.
    pub fn agent_hello(&self, id: &str, hello: &Value) {
        let restore = {
            let mut agents = locked(&self.inner.agents);
            let chosen = agents.chosen.clone();
            let Some(agent) = agents.list.iter_mut().find(|agent| agent.id == id) else { return };
            agent.name = hello
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .map(|name| name.chars().take(64).collect());
            let mut restore = None;
            if agent.kind == Needs::Screen {
                agent.monitors = hello
                    .get("monitors")
                    .and_then(Value::as_array)
                    .map(|areas| areas.iter().filter_map(monitor_area).collect())
                    .unwrap_or_default();
                agent.monitor = hello.get("monitor").and_then(Value::as_i64);
                let earlier = agent.name.clone().and_then(|name| chosen.get(&(agent.kind, name)).copied());
                let changed = |index: &i64| Some(*index) != agent.monitor && agent.has_monitor(*index);
                if let Some(index) = earlier.filter(changed) {
                    agent.monitor = Some(index);
                    restore = Some((agent.events.clone(), index));
                }
            }
            restore
        };
        if let Some((events, index)) = restore {
            let _ = events.send(json!({"event": "monitor", "monitor": index}));
        }
        self.inner.notify();
    }

    pub async fn agent_left(&self, kind: Needs, id: &str) {
        {
            let _guard = self.inner.lock.lock().await;
            locked(&self.inner.agents).list.retain(|agent| agent.id != id);
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
    /// Change what the light does for each reason the computer is away, e.g. from a settings page.
    pub fn set_away_looks(&self, looks: HashMap<String, AwayLook>) {
        *locked(&self.inner.away_looks) = looks;
    }

    pub fn remote_audio(&self) -> Arc<AudioSource> {
        self.inner.remote_audio.clone()
    }
}

const COMMANDS: [&str; 17] = [
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
    "monitor",
    "away",
    "back",
];

/// One monitor an agent lists, kept only when it says its index and size.
fn monitor_area(area: &Value) -> Option<Value> {
    let number = |key: &str| area.get(key).and_then(Value::as_i64);
    Some(json!({"index": number("index")?, "width": number("width")?, "height": number("height")?}))
}

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

fn follows(effect: &Value, kind: Needs) -> bool {
    effect.get("name").and_then(Value::as_str).and_then(catalog::lookup).is_some_and(|info| info.follows(kind))
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
            "away": locked(&self.away).clone(),
            "audio": source(audio_agents),
            "screen": source(screen_agents),
            "agents": {"audio": audio_agents, "screen": screen_agents},
            "agentInfo": self.agent_info(),
            "watchers": self.watchers.load(Ordering::SeqCst),
        });
        if self.local_screen.as_ref().is_some_and(|capture| capture.asks()) {
            state["localScreenAsks"] = json!(true);
        }
        if !self.local_monitors.is_empty() {
            state["localMonitors"] = json!(self.local_monitors);
            state["localMonitor"] = json!(self.local_monitor.load(Ordering::SeqCst));
        }
        if let (Value::Object(state), Value::Object(plan)) = (&mut state, locked(&self.plan).to_json()) {
            state.extend(plan);
        }
        state
    }

    fn agent_info(&self) -> Value {
        let agents = locked(&self.agents);
        let of = |kind: Needs| -> Vec<Value> {
            agents.list.iter().filter(|agent| agent.kind == kind).map(Agent::info).collect()
        };
        json!({"audio": of(Needs::Audio), "screen": of(Needs::Screen)})
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
        self.effect_running.load(Ordering::SeqCst)
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
        let mut held = self.halt_effect().await;
        if !self.bulb.is_connected() {
            self.release(held).await;
            return Ok(());
        }
        let plan = locked(&self.plan).clone();
        let away = locked(&self.away).clone();
        let shown = async {
            if let Some(reason) = away {
                return self.show_away(&reason, &plan).await;
            }
            if self.bulb.brightness() != plan.brightness {
                self.bulb.set_brightness(plan.brightness).await?;
            }
            if !plan.on {
                self.bulb.turn_off(fade).await
            } else if let Some(effect) = &plan.effect {
                self.start_effect(effect, duration, &mut held).await
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
        let shown = shown.await;
        self.release(held).await; // whatever the light no longer follows
        match shown {
            Err(error) if error.is_link_error() => {
                self.link_lost(&error).await;
                Ok(())
            }
            other => other,
        }
    }

    /// The look of an away computer, written to the bulb only: the plan stays as it is.
    async fn show_away(&self, reason: &str, plan: &Plan) -> Result<()> {
        let look = locked(&self.away_looks).get(reason).copied();
        if look == Some(AwayLook::Off) || !plan.on {
            self.bulb.turn_off(false).await
        } else {
            self.bulb.set_brightness(plan.brightness.min(AWAY_DIM)).await
        }
    }

    async fn link_lost(&self, error: &Error) {
        log::info!("lost the bulb: {error}");
        self.bulb.disconnect().await;
        self.notify();
    }

    async fn commit(self: &Arc<Self>, fade: bool, duration: Option<f64>) -> Result<()> {
        *locked(&self.away) = None; // somebody is using the light
        self.adopt_on_connect.store(false, Ordering::SeqCst); // a request made while the bulb is away wins
        self.save();
        self.apply(fade, duration).await
    }

    // --- effects ----------------------------------------------------------------------

    async fn start_effect(self: &Arc<Self>, spec: &Value, duration: Option<f64>, held: &mut Held) -> Result<()> {
        let name = spec.get("name").and_then(Value::as_str).unwrap_or_default();
        let params = spec.get("params").and_then(Value::as_object);
        let mut sources = Sources::default();
        if follows(spec, Needs::Audio) {
            sources.audio = Some(self.audio_source(held).await);
        }
        if follows(spec, Needs::Screen) {
            sources.screen = Some(self.screen_source(held).await);
        }
        let effect = catalog::create(name, params, &sources)?;
        let inner = self.clone();
        self.effect_running.store(true, Ordering::SeqCst);
        let task = tokio::spawn(async move {
            inner.run_effect(effect, duration).await;
            // it ended by itself (a stopped effect never gets here): tell the clients
            inner.effect_running.store(false, Ordering::SeqCst);
            inner.notify();
        });
        *locked(&self.effect_task) = Some(task);
        Ok(())
    }

    async fn audio_source(&self, held: &mut Held) -> Arc<AudioSource> {
        if locked(&self.feeds).audio_agents > 0 {
            return self.remote_audio.clone();
        }
        let Some(capture) = &self.local_audio else {
            log::warn!("nothing to capture sound with here: staying dark until an agent brings a feed");
            return self.remote_audio.clone();
        };
        let source = match held.audio.take().filter(|_| capture.alive()) {
            Some(source) => source,
            None => {
                let source = AudioSource::new(self.clock.clone());
                if let Err(error) = capture.start(source.clone()).await {
                    log::warn!("sound effect has no input: {error}");
                    return self.remote_audio.clone();
                }
                source
            }
        };
        locked(&self.feeds).audio_local = Some(source.clone());
        source
    }

    async fn screen_source(&self, held: &mut Held) -> Arc<ScreenSource> {
        if locked(&self.feeds).screen_agents > 0 {
            return self.remote_screen.clone();
        }
        let Some(capture) = &self.local_screen else { return self.remote_screen.clone() };
        let monitor = self.local_monitor.load(Ordering::SeqCst);
        let source = match held.screen.take() {
            Some((source, watched)) if watched == monitor && capture.alive() => source,
            watching => {
                if watching.is_some() {
                    capture.stop().await; // it watches another monitor
                }
                let source = ScreenSource::new();
                if let Err(error) = capture.start(source.clone(), monitor).await {
                    log::warn!("screen effect has no input: {error}");
                    return self.remote_screen.clone();
                }
                source
            }
        };
        locked(&self.feeds).screen_local = Some((source.clone(), monitor));
        source
    }

    fn hold_sources(&self) -> Held {
        let mut feeds = locked(&self.feeds);
        Held { audio: feeds.audio_local.take(), screen: feeds.screen_local.take() }
    }

    async fn release(&self, held: Held) {
        if held.audio.is_some() {
            if let Some(capture) = &self.local_audio {
                capture.stop().await;
            }
        }
        if held.screen.is_some() {
            if let Some(capture) = &self.local_screen {
                capture.stop().await;
            }
        }
    }

    async fn release_sources(&self) {
        let held = self.hold_sources();
        self.release(held).await;
    }

    async fn run_effect(self: &Arc<Self>, effect: Effect, duration: Option<f64>) {
        // a slow bearer gets fewer frames, so requests do not queue up behind them
        let fps = self.options.fps.min(self.bulb.bearer().fps());
        if let Err(error) = play(&self.bulb, effect, duration, fps).await {
            self.effect_running.store(false, Ordering::SeqCst);
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
        let held = self.halt_effect().await;
        self.release(held).await;
    }

    /// End the effect and hand over the captures it followed, for the next effect or to release.
    async fn halt_effect(&self) -> Held {
        let task = locked(&self.effect_task).take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        self.effect_running.store(false, Ordering::SeqCst);
        self.hold_sources()
    }

    async fn feed_changed(self: &Arc<Self>, kind: Needs) {
        let effect = locked(&self.plan).effect.clone();
        if self.playing() && effect.as_ref().is_some_and(|effect| follows(effect, kind)) {
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

    /// Tell a screen agent which of its monitors to watch.
    async fn choose_monitor(self: &Arc<Self>, request: &Value) -> Result<Value> {
        let target = as_text(field(request, "agent")?);
        let index = as_number(field(request, "index")?, "index")? as i64;
        if target == "local" {
            if let Some(capture) = self.local_screen.as_ref().filter(|capture| capture.asks()) {
                capture.choose_again().await;
                self.feed_changed(Needs::Screen).await; // an effect following the screen asks right away
                return Ok(json!({}));
            }
            if self.local_monitors.is_empty() {
                return Err(invalid("this machine cannot capture its screen"));
            }
            if index != 0 && !self.local_monitors.iter().any(|area| area["index"] == index) {
                let last = self.local_monitors.iter().filter_map(|area| area["index"].as_i64()).max().unwrap_or(1);
                return Err(invalid(format!("no monitor {index}; this machine has 1 to {last}")));
            }
            self.local_monitor.store(index, Ordering::SeqCst);
            self.feed_changed(Needs::Screen).await; // an effect following the screen restarts on it
            return Ok(json!({}));
        }
        let mut agents = locked(&self.agents);
        let Some(agent) = agents.list.iter_mut().find(|agent| agent.id == target) else {
            return Err(invalid(format!("no such agent: '{target}'")));
        };
        if agent.monitors.is_empty() {
            return Err(invalid("that agent cannot change its monitor"));
        }
        if !agent.has_monitor(index) {
            let last = agent.monitors.iter().filter_map(|area| area["index"].as_i64()).max().unwrap_or(1);
            return Err(invalid(format!("no monitor {index}; this machine has 1 to {last}")));
        }
        agent.monitor = Some(index);
        let _ = agent.events.send(json!({"event": "monitor", "monitor": index}));
        let remembered = agent.name.clone().map(|name| ((agent.kind, name), index));
        if let Some((key, index)) = remembered {
            agents.chosen.insert(key, index);
        }
        Ok(json!({}))
    }

    async fn run(self: &Arc<Self>, command: &str, request: &Value) -> Result<Value> {
        let fade = truthy(request.get("fade"));
        match command {
            "monitor" => self.choose_monitor(request).await,
            "away" => {
                let reason = as_text(field(request, "reason")?);
                if !AWAY_REASONS.contains(&reason.as_str()) {
                    return Err(invalid(format!("reason must be one of {}", AWAY_REASONS.join(", "))));
                }
                if locked(&self.away_looks).contains_key(&reason) {
                    *locked(&self.away) = Some(reason); // a later reason replaces an earlier one: locked, then asleep
                    self.apply(false, None).await?;
                }
                Ok(json!({}))
            }
            "back" => {
                if locked(&self.away).take().is_some() {
                    self.apply(true, None).await?;
                }
                Ok(json!({}))
            }
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
                *locked(&self.away) = None;
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
    /// What this connection supplies as an agent, and under which id.
    feeding: HashMap<Needs, String>,
    subscription: Option<Subscription>,
    events: mpsc::UnboundedSender<Value>,
    told: mpsc::UnboundedReceiver<Value>,
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
        let (events, told) = mpsc::unbounded_channel();
        Self { service, token, trusted, feeding: HashMap::new(), subscription: None, events, told }
    }

    /// State updates once the client has subscribed, and what the service tells this client as an agent.
    pub fn watch(&mut self) -> (Option<&mut watch::Receiver<Value>>, &mut mpsc::UnboundedReceiver<Value>) {
        (self.subscription.as_mut().map(|subscription| &mut subscription.updates), &mut self.told)
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
            "hello" => match request.get("kind").and_then(Value::as_str) {
                Some("audio") => Some(Needs::Audio),
                Some("screen") => Some(Needs::Screen),
                _ => None,
            },
            _ => None,
        } {
            if !self.feeding.contains_key(&kind) {
                let id = self.service.agent_joined(kind, self.events.clone()).await;
                self.feeding.insert(kind, id);
            }
            if command == "hello" {
                self.service.agent_hello(&self.feeding[&kind], &request);
            } else {
                self.service.push(kind, &request);
            }
            return Outcome { reply: None, close: false }; // an agent's stream and greeting are not acknowledged
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
        for (kind, id) in std::mem::take(&mut self.feeding) {
            self.service.agent_left(kind, &id).await;
        }
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}
