//! The background service against a fake bulb; mirrors `tests/test_service.py`.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chsmartbulb_core::audio::AudioSource;
use chsmartbulb_core::catalog::Needs;
use chsmartbulb_core::screen::ScreenSource;
use chsmartbulb_core::service::{AudioCapture, AwayLook, Options, ScreenCapture, Service, Session};
use chsmartbulb_core::sim::SimulatedBulb as FakeBulb;
use chsmartbulb_core::{Bulb, Color, Result};
use serde_json::{json, Value};

fn options() -> Options {
    Options { fps: 200.0, ..Options::default() }
}

async fn attached(fake: &FakeBulb, options: Options) -> Service {
    let service = Service::builder(Bulb::new(fake.connector())).options(options).build();
    service.attach().await.unwrap();
    service
}

async fn until(mut condition: impl FnMut() -> bool) {
    for _ in 0..400 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("condition was never met");
}

async fn ask(service: &Service, request: Value) -> Value {
    service.handle(&request).await
}

#[tokio::test]
async fn colour_brightness_and_power_requests_drive_the_bulb() {
    let fake = FakeBulb::new();
    let service = attached(&fake, options()).await;
    assert_eq!(ask(&service, json!({"cmd": "color", "color": "#ff0064", "brightness": 0.5})).await["ok"], true);
    let last = fake.last_light();
    assert_eq!((last.r, last.b), (128, 50));
    ask(&service, json!({"cmd": "off"})).await;
    assert_eq!(fake.with(|s| s.channels), [0; 5]);
    ask(&service, json!({"cmd": "brightness", "level": 1.0})).await;
    assert_eq!(fake.with(|s| s.channels), [0; 5]); // dimming must not switch the light on
    ask(&service, json!({"cmd": "on", "fade": true})).await;
    let last = fake.last_light();
    assert_eq!((last.r, last.fade), (255, 1));
    let status = ask(&service, json!({"cmd": "status"})).await;
    assert_eq!((status["connected"].clone(), status["on"].clone()), (json!(true), json!(true)));
    assert_eq!((status["color"].clone(), status["bulb"].clone()), (json!("#ff0064"), json!("#ff0064")));
    service.close().await;
}

#[tokio::test]
async fn bad_requests_are_reported_not_raised() {
    let fake = FakeBulb::new();
    let service = attached(&fake, options()).await;
    for (request, fragment) in [
        (json!({"cmd": "dance"}), "unknown command 'dance'"),
        (json!({"cmd": "color"}), "missing field 'color'"),
        (json!({"cmd": "color", "color": "nope"}), "hex"),
        (json!({"cmd": "brightness", "level": 3}), "0..1"),
        (json!({"cmd": "effect", "name": "disco"}), "unknown effect"),
        (json!({"cmd": "effect", "name": "hue", "params": {"color": "red"}}), "no parameter"),
        (json!({"cmd": "effect", "name": "music", "params": {"delay": 9}}), "delay"),
        (json!({"cmd": "native", "name": "fixed"}), "unknown native effect"),
    ] {
        let reply = ask(&service, request.clone()).await;
        assert_eq!(reply["ok"], false, "{request}");
        assert!(reply["error"].as_str().unwrap().contains(fragment), "{reply}");
    }
    let status = ask(&service, json!({"cmd": "status"})).await;
    assert_eq!((status["effect"].clone(), status["playing"].clone()), (Value::Null, json!(false)));
    service.close().await;
}

#[tokio::test]
async fn effect_runs_in_the_background_and_stop_returns_to_the_plain_colour() {
    let fake = FakeBulb::new();
    let service = attached(&fake, options()).await;
    ask(&service, json!({"cmd": "color", "color": "#0000ff"})).await;
    let sent = fake.sent();
    let reply = ask(&service, json!({"cmd": "effect", "name": "strobe", "params": {"color": "red", "hz": 50}})).await;
    assert_eq!(reply["ok"], true);
    until(|| fake.sent() > sent + 3).await;
    assert_eq!(ask(&service, json!({"cmd": "status"})).await["playing"], true);
    ask(&service, json!({"cmd": "brightness", "level": 0.5})).await; // dims without restarting the effect
    until(|| fake.last_light().r == 128).await;
    ask(&service, json!({"cmd": "stop"})).await;
    assert_eq!(ask(&service, json!({"cmd": "status"})).await["playing"], false);
    let last = fake.last_light();
    assert_eq!((last.r, last.b), (0, 128));
    service.close().await;
}

#[tokio::test]
async fn finite_effect_returns_to_the_plain_colour_when_done() {
    let fake = FakeBulb::new();
    let service = attached(&fake, options()).await;
    ask(&service, json!({"cmd": "color", "color": "#0000ff"})).await;
    ask(&service, json!({"cmd": "effect", "name": "hue", "duration": 0.05})).await;
    service.wait_effect().await;
    let status = ask(&service, json!({"cmd": "status"})).await;
    assert_eq!(status["effect"], Value::Null);
    assert_eq!(status["playing"], false);
    assert_eq!(status["bulb"], "#0000ff");
    service.close().await;
}

#[derive(Default)]
struct FakeCapture {
    running: AtomicBool,
    starts: AtomicUsize,
}

#[async_trait]
impl AudioCapture for FakeCapture {
    async fn start(&self, _source: Arc<AudioSource>) -> Result<()> {
        self.running.store(true, Ordering::SeqCst);
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn sound_effect_starts_and_stops_the_audio_capture() {
    let fake = FakeBulb::new();
    let capture = Arc::new(FakeCapture::default());
    let service =
        Service::builder(Bulb::new(fake.connector())).options(options()).audio_capture(capture.clone()).build();
    service.attach().await.unwrap();
    let rejected = ask(&service, json!({"cmd": "effect", "name": "music", "params": {"delay": 9}})).await;
    assert_eq!(rejected["ok"], false);
    assert!(!capture.running.load(Ordering::SeqCst));
    ask(&service, json!({"cmd": "effect", "name": "music"})).await;
    assert!(capture.running.load(Ordering::SeqCst));
    // retuning the effect, or moving to another that listens, keeps the capture open
    ask(&service, json!({"cmd": "effect", "name": "music", "params": {"delay": 0.2}})).await;
    ask(&service, json!({"cmd": "effect", "name": "spectrum"})).await;
    assert!(capture.running.load(Ordering::SeqCst));
    assert_eq!(capture.starts.load(Ordering::SeqCst), 1);
    ask(&service, json!({"cmd": "color", "color": "red"})).await;
    assert!(!capture.running.load(Ordering::SeqCst));
    // an agent takes over from the local capture
    ask(&service, json!({"cmd": "effect", "name": "music"})).await;
    let (told, _heard) = tokio::sync::mpsc::unbounded_channel();
    let agent = service.agent_joined(Needs::Audio, told).await;
    assert!(!capture.running.load(Ordering::SeqCst));
    assert_eq!(service.state()["audio"], "agent");
    service.agent_left(Needs::Audio, &agent).await;
    assert!(capture.running.load(Ordering::SeqCst));
    service.close().await;
    assert!(!capture.running.load(Ordering::SeqCst));
}

/// Records the monitors it was started on.
#[derive(Default)]
struct FakeScreen {
    watched: std::sync::Mutex<Vec<i64>>,
    running: AtomicBool,
    /// Stands for a desktop that has the user choose the screen.
    asks: bool,
}

#[async_trait]
impl ScreenCapture for FakeScreen {
    fn monitors(&self) -> Vec<Value> {
        if self.asks {
            return Vec::new();
        }
        vec![json!({"index": 1, "width": 1920, "height": 1080}), json!({"index": 2, "width": 1280, "height": 720})]
    }

    fn asks(&self) -> bool {
        self.asks
    }

    async fn choose_again(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    fn alive(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    async fn start(&self, source: Arc<ScreenSource>, monitor: i64) -> Result<()> {
        self.watched.lock().unwrap().push(monitor);
        self.running.store(true, Ordering::SeqCst);
        source.push(Color::rgb(10, 0, 0));
        Ok(())
    }

    async fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn the_services_own_screen_follows_the_chosen_monitor() {
    let fake = FakeBulb::new();
    let screen = Arc::new(FakeScreen::default());
    let service =
        Service::builder(Bulb::new(fake.connector())).options(options()).screen_capture(screen.clone()).build();
    service.attach().await.unwrap();
    let status = ask(&service, json!({"cmd": "status"})).await;
    assert_eq!(status["localMonitors"], json!(screen.monitors()));
    assert_eq!(status["localMonitor"], 1);
    ask(&service, json!({"cmd": "effect", "name": "screen", "params": {"smoothing": 0}})).await;
    // retuning keeps the capture; another monitor restarts it there
    ask(&service, json!({"cmd": "effect", "name": "screen", "params": {"smoothing": 0.1}})).await;
    assert_eq!(*screen.watched.lock().unwrap(), [1]);
    let chosen = ask(&service, json!({"cmd": "monitor", "agent": "local", "index": 2})).await;
    assert_eq!(chosen["ok"], true);
    assert_eq!(*screen.watched.lock().unwrap(), [1, 2]);
    assert!(screen.running.load(Ordering::SeqCst));
    assert_eq!(ask(&service, json!({"cmd": "status"})).await["localMonitor"], 2);
    let bad = ask(&service, json!({"cmd": "monitor", "agent": "local", "index": 9})).await;
    assert_eq!(bad["ok"], false);
    ask(&service, json!({"cmd": "color", "color": "red"})).await;
    assert!(!screen.running.load(Ordering::SeqCst));
    service.close().await;
}

#[tokio::test]
async fn a_desktop_that_asks_for_the_screen_is_asked_again_on_request() {
    let fake = FakeBulb::new();
    let screen = Arc::new(FakeScreen { asks: true, ..FakeScreen::default() });
    let service =
        Service::builder(Bulb::new(fake.connector())).options(options()).screen_capture(screen.clone()).build();
    service.attach().await.unwrap();
    let status = ask(&service, json!({"cmd": "status"})).await;
    assert_eq!((status["localScreenAsks"].clone(), status.get("localMonitors")), (json!(true), None));
    // nothing follows the screen yet: the choice is forgotten, nobody is asked
    let chosen = ask(&service, json!({"cmd": "monitor", "agent": "local", "index": 0})).await;
    assert_eq!(chosen["ok"], true);
    assert!(screen.watched.lock().unwrap().is_empty());
    ask(&service, json!({"cmd": "effect", "name": "screen"})).await;
    assert_eq!(screen.watched.lock().unwrap().len(), 1);
    // while the effect runs, the capture starts over, which is when the desktop asks
    ask(&service, json!({"cmd": "monitor", "agent": "local", "index": 0})).await;
    assert_eq!(screen.watched.lock().unwrap().len(), 2);
    assert!(screen.running.load(Ordering::SeqCst));
    service.close().await;
}

#[tokio::test]
async fn a_service_that_cannot_capture_its_screen_offers_no_monitors() {
    let fake = FakeBulb::new();
    let service = attached(&fake, options()).await;
    let status = ask(&service, json!({"cmd": "status"})).await;
    assert_eq!(status.get("localMonitors"), None);
    let refused = ask(&service, json!({"cmd": "monitor", "agent": "local", "index": 1})).await;
    assert_eq!(refused["ok"], false);
    service.close().await;
}

#[tokio::test]
async fn an_effect_of_screen_and_sound_follows_both() {
    let fake = FakeBulb::new();
    let capture = Arc::new(FakeCapture::default());
    let service =
        Service::builder(Bulb::new(fake.connector())).options(options()).audio_capture(capture.clone()).build();
    service.attach().await.unwrap();
    assert_eq!(ask(&service, json!({"cmd": "effect", "name": "screensound"})).await["ok"], true);
    assert!(capture.running.load(Ordering::SeqCst));
    // a screen agent restarts it on its feed, and the sound is still listened to here
    let (told, _heard) = tokio::sync::mpsc::unbounded_channel();
    let agent = service.agent_joined(Needs::Screen, told).await;
    assert!(capture.running.load(Ordering::SeqCst));
    assert_eq!(service.state()["playing"], true);
    service.agent_left(Needs::Screen, &agent).await;
    ask(&service, json!({"cmd": "stop"})).await;
    assert!(!capture.running.load(Ordering::SeqCst));
    service.close().await;
}

#[tokio::test]
async fn native_effect_and_bulb_queries() {
    let fake = FakeBulb::new();
    fake.with(|s| s.chunk = Some(20)); // answers arrive in pieces, as BLE notifications do
    let service = attached(&fake, options()).await;
    ask(&service, json!({"cmd": "native", "name": "breathing", "color": "#00ff00", "speed": 4})).await;
    let last = fake.last_light();
    assert_eq!((last.effect, last.g, last.speed), (0x52, 255, 0x41));
    let info = ask(&service, json!({"cmd": "info"})).await;
    assert_eq!((info["name"].clone(), info["model"].clone()), (json!("SmartBulb Bluetooth"), json!("BL04")));
    let timers = ask(&service, json!({"cmd": "timers"})).await;
    let listed: Vec<(String, bool)> = timers["timers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["name"].as_str().unwrap().to_string(), t["enabled"].as_bool().unwrap()))
        .collect();
    assert_eq!(listed, [("power off".to_string(), true), ("power on".to_string(), false)]);
    let changed = ask(&service, json!({"cmd": "timer", "index": 6, "enabled": false})).await;
    assert_eq!(changed["timer"]["enabled"], false);
    let raw = ask(&service, json!({"cmd": "raw", "hex": "01fe0000510210000000008000000080"})).await;
    assert_eq!(raw["answer"], "01fe000041021000470c000000000000");
    service.close().await;
}

#[tokio::test]
async fn state_is_remembered_and_restored_after_a_restart() {
    let dir = std::env::temp_dir().join(format!("chsmartbulb-test-{}", std::process::id()));
    let state_path = dir.join("state").join("state.json");
    let _ = std::fs::remove_dir_all(&dir);
    let with_state = || Options { state_path: Some(state_path.clone()), ..options() };

    let fake = FakeBulb::new();
    let service = attached(&fake, with_state()).await;
    ask(&service, json!({"cmd": "color", "color": "#102030", "brightness": 0.5})).await;
    service.close().await;
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    assert_eq!(saved["color"], "#102030");

    let fake = FakeBulb::new(); // a bulb that came back white after losing power
    fake.with(|s| s.channels = [0, 0, 0, 255, 0]);
    let service = Service::builder(Bulb::new(fake.connector())).options(with_state()).build();
    service.start();
    until(|| fake.sent() > 0 && fake.last_light().w == 0).await;
    let last = fake.last_light();
    assert_eq!((last.r, last.g, last.b), (8, 16, 24));
    service.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn supervisor_reconnects_and_resumes_the_effect() {
    let fake = FakeBulb::new();
    fake.with(|s| s.fail_open = true);
    let quick =
        Options { retry_delay: Duration::from_millis(10), poll_interval: Duration::from_millis(10), ..options() };
    let service = Service::builder(Bulb::new(fake.connector())).options(quick).build();
    service.start();
    tokio::time::sleep(Duration::from_millis(30)).await;
    let reply = ask(&service, json!({"cmd": "effect", "name": "strobe", "params": {"hz": 50}})).await;
    assert_eq!(reply["ok"], true); // accepted while the bulb is away
    let status = ask(&service, json!({"cmd": "status"})).await;
    assert_eq!(status["connected"], false);
    assert_eq!(status["problem"], "simulated: host is down");
    assert!(ask(&service, json!({"cmd": "info"})).await["error"].as_str().unwrap().contains("not connected"));

    fake.with(|s| s.fail_open = false); // the bulb is switched on
    until(|| fake.sent() > 3).await;

    fake.drop_link(); // and loses power again
    let sent = fake.sent();
    until(|| fake.with(|s| s.opened) >= 2 && fake.sent() > sent + 3).await;
    assert_eq!(ask(&service, json!({"cmd": "status"})).await["playing"], true);
    service.close().await;
}

#[tokio::test]
async fn reconnect_request_retries_at_once() {
    let fake = FakeBulb::new();
    fake.with(|s| s.fail_open = true);
    let slow = Options { retry_delay: Duration::from_secs(30), ..options() };
    let service = Service::builder(Bulb::new(fake.connector())).options(slow).build();
    let mut subscription = service.subscribe();
    service.start();
    until(|| subscription.updates.borrow()["link"] == "waiting").await;
    fake.with(|s| s.fail_open = false); // back in range, long before the next attempt is due
    assert_eq!(ask(&service, json!({"cmd": "reconnect"})).await["ok"], true);
    until(|| subscription.updates.borrow_and_update()["link"] == "connected").await;
    assert_eq!(subscription.updates.borrow()["problem"], Value::Null);
    service.close().await;
}

#[tokio::test]
async fn effects_reply_describes_every_parameter_and_the_native_effects() {
    let fake = FakeBulb::new();
    let service = attached(&fake, options()).await;
    let reply = ask(&service, json!({"cmd": "effects"})).await;
    let names: Vec<&str> = reply["native"]["names"].as_array().unwrap().iter().map(|n| n.as_str().unwrap()).collect();
    assert!(names.contains(&"breathing") && !names.contains(&"fixed"));
    assert_eq!(reply["native"]["speed"], json!([0, 15]));
    assert_eq!(reply["effects"].as_array().unwrap().len(), 28);
    service.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_sleep_timer_dims_the_light_and_switches_it_off() {
    let fake = FakeBulb::new();
    let service = attached(&fake, options()).await;
    ask(&service, json!({"cmd": "color", "color": "#ff0000", "brightness": 0.8})).await;
    assert_eq!(ask(&service, json!({"cmd": "sleep", "minutes": 10})).await["ok"], true);
    let state = service.state();
    assert_eq!((&state["sleep"]["minutes"], &state["sleep"]["left"]), (&json!(10.0), &json!(600.0)));
    tokio::time::sleep(Duration::from_secs(301)).await;
    let half = fake.last_light().r;
    assert!((98..=104).contains(&half), "{half}"); // half of 0.8 of 255
    assert_eq!(service.state()["brightness"], 0.8); // the plan keeps what the light comes back to
    ask(&service, json!({"cmd": "color", "color": "#0000ff"})).await; // changing the light does not end it
    assert!((98..=104).contains(&fake.last_light().b));
    tokio::time::sleep(Duration::from_secs(300)).await;
    assert_eq!(fake.with(|s| s.channels), [0; 5]);
    let state = service.state();
    assert_eq!((&state["on"], &state["sleep"]), (&json!(false), &Value::Null));
    ask(&service, json!({"cmd": "on"})).await;
    assert_eq!(fake.last_light().b, 204); // back at full: 0.8 of 255
    service.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_sleep_timer_can_be_called_off() {
    let fake = FakeBulb::new();
    let service = attached(&fake, options()).await;
    ask(&service, json!({"cmd": "color", "color": "#ff0000"})).await;
    ask(&service, json!({"cmd": "sleep", "minutes": 2})).await;
    tokio::time::sleep(Duration::from_secs(61)).await;
    assert!(fake.last_light().r < 135);
    ask(&service, json!({"cmd": "sleep", "minutes": 0})).await;
    assert_eq!(fake.last_light().r, 255);
    assert_eq!(service.state()["sleep"], Value::Null);
    tokio::time::sleep(Duration::from_secs(120)).await;
    assert_eq!(fake.last_light().r, 255); // and it stays on

    ask(&service, json!({"cmd": "sleep", "minutes": 2})).await;
    ask(&service, json!({"cmd": "off"})).await; // switching it off by hand ends the timer too
    assert_eq!(service.state()["sleep"], Value::Null);
    let reply = ask(&service, json!({"cmd": "sleep", "minutes": 2})).await;
    assert!(reply["error"].as_str().unwrap().contains("off already"), "{reply}");
    let reply = ask(&service, json!({"cmd": "sleep", "minutes": 9000})).await;
    assert!(reply["error"].as_str().unwrap().contains("0..480"), "{reply}");
    service.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_sleep_timer_dims_a_running_effect() {
    let fake = FakeBulb::new();
    let service = attached(&fake, options()).await;
    ask(&service, json!({"cmd": "effect", "name": "strobe", "params": {"color": "red", "hz": 2, "duty": 0.9}})).await;
    ask(&service, json!({"cmd": "sleep", "minutes": 1})).await;
    tokio::time::sleep(Duration::from_millis(30_100)).await;
    assert_eq!(service.state()["playing"], true);
    let half = fake.last_light().r;
    assert!((120..=135).contains(&half), "{half}");
    tokio::time::sleep(Duration::from_secs(31)).await;
    let state = service.state();
    assert_eq!((&state["on"], &state["playing"]), (&json!(false), &json!(false)));
    assert_eq!(fake.with(|s| s.channels), [0; 5]);
    service.close().await;
}

#[tokio::test]
async fn subscribers_hear_changes_and_count_watchers() {
    let fake = FakeBulb::new();
    let service = attached(&fake, options()).await;
    let mut first = service.subscribe();
    assert!(!first.updates.has_changed().unwrap()); // a newcomer is told the state in its reply
    let second = service.subscribe();
    assert_eq!(first.updates.borrow_and_update()["watchers"], 2);
    ask(&service, json!({"cmd": "color", "color": "#00ff00"})).await;
    assert_eq!(first.updates.borrow_and_update()["color"], "#00ff00");
    ask(&service, json!({"cmd": "status"})).await;
    assert!(!first.updates.has_changed().unwrap()); // nothing changed
    ask(&service, json!({"cmd": "effect", "name": "hue", "duration": 0.02})).await;
    assert_eq!(first.updates.borrow_and_update()["effect"]["name"], "hue");
    until(|| first.updates.borrow()["playing"] == false).await; // ran out by itself
    assert_eq!(first.updates.borrow()["effect"], Value::Null);
    drop(second);
    assert_eq!(first.updates.borrow_and_update()["watchers"], 1);
    service.close().await;
}

#[tokio::test]
async fn sessions_follow_the_socket_protocol() {
    let fake = FakeBulb::new();
    let service = attached(&fake, options()).await;

    let mut stranger = Session::new(service.clone(), Some("secret".into()));
    let outcome = stranger.receive(r#"{"cmd": "status", "id": 1}"#).await;
    assert_eq!(outcome.reply.unwrap(), json!({"ok": false, "error": "not authorised", "id": 1}));
    assert!(outcome.close);

    let mut client = Session::new(service.clone(), Some("secret".into()));
    assert!(client.receive(r#"{"cmd": "auth", "token": "wrong"}"#).await.close);
    let mut client = Session::new(service.clone(), Some("secret".into()));
    let outcome = client.receive(r#"{"cmd": "auth", "token": "secret"}"#).await;
    assert_eq!(outcome.reply.unwrap()["ok"], true);
    let subscribed = client.receive(r#"{"cmd": "subscribe", "id": 7}"#).await.reply.unwrap();
    assert_eq!(
        (subscribed["ok"].clone(), subscribed["id"].clone(), subscribed["watchers"].clone()),
        (json!(true), json!(7), json!(1))
    );
    assert!(client.receive("not json").await.reply.unwrap()["error"].as_str().unwrap().starts_with("bad request"));

    let mut agent = Session::new(service.clone(), None);
    let outcome = agent.receive(r#"{"cmd": "audio", "levels": [1, 0.5, 0], "onset": 3}"#).await;
    assert!(outcome.reply.is_none()); // a stream is not answered
    let updates = client.updates().unwrap();
    assert_eq!(updates.borrow_and_update()["agents"], json!({"audio": 1, "screen": 0}));
    agent.close().await;
    assert_eq!(client.updates().unwrap().borrow_and_update()["agents"]["audio"], 0);
    client.close().await;
    service.close().await;
}

#[tokio::test]
async fn agents_say_who_they_are_and_which_monitor_they_choose() {
    let fake = FakeBulb::new();
    let service = attached(&fake, options()).await;
    let hello = json!({
        "cmd": "hello", "kind": "screen", "name": "desk", "monitor": 1,
        "monitors": [{"index": 1, "width": 1920, "height": 1080}, {"index": 2, "width": 1280, "height": 1024}],
    })
    .to_string();

    let mut agent = Session::new(service.clone(), None);
    assert!(agent.receive(&hello).await.reply.is_none()); // a greeting is not answered
    let info = service.state()["agentInfo"]["screen"].clone();
    assert_eq!((info[0]["name"].clone(), info[0]["monitor"].clone()), (json!("desk"), json!(1)));
    assert_eq!(info[0]["monitors"][1]["width"], 1280);
    let id = info[0]["id"].as_str().unwrap().to_string();

    assert_eq!(ask(&service, json!({"cmd": "monitor", "agent": id, "index": 2})).await, json!({"ok": true}));
    assert_eq!(agent.watch().1.recv().await.unwrap(), json!({"event": "monitor", "monitor": 2}));
    assert_eq!(service.state()["agentInfo"]["screen"][0]["monitor"], 2);

    let bad = ask(&service, json!({"cmd": "monitor", "agent": id, "index": 7})).await;
    assert_eq!(bad["error"], "no monitor 7; this machine has 1 to 2");
    let unknown = ask(&service, json!({"cmd": "monitor", "agent": "nobody", "index": 1})).await;
    assert_eq!(unknown, json!({"ok": false, "error": "no such agent: 'nobody'"}));
    let local = ask(&service, json!({"cmd": "monitor", "agent": "local", "index": 1})).await;
    assert_eq!(local["error"], "this machine cannot capture its screen");

    agent.close().await; // a returning agent of the same name gets its choice back
    assert_eq!(service.state()["agentInfo"]["screen"], json!([]));
    let mut again = Session::new(service.clone(), None);
    again.receive(&hello).await;
    assert_eq!(again.watch().1.recv().await.unwrap(), json!({"event": "monitor", "monitor": 2}));
    again.close().await;

    // one that never says hello still counts, but cannot choose
    let mut silent = Session::new(service.clone(), None);
    silent.receive(r##"{"cmd": "screen", "color": "#102030"}"##).await;
    let info = service.state()["agentInfo"]["screen"].clone();
    assert_eq!((info[0]["name"].clone(), info[0]["monitors"].clone()), (Value::Null, json!([])));
    let id = info[0]["id"].as_str().unwrap().to_string();
    let reply = ask(&service, json!({"cmd": "monitor", "agent": id, "index": 1})).await;
    assert_eq!(reply["error"], "that agent cannot change its monitor");
    silent.close().await;
    service.close().await;
}

fn away_options() -> Options {
    let looks = [("lock", AwayLook::Dim), ("sleep", AwayLook::Off)];
    Options { away_looks: looks.into_iter().map(|(reason, look)| (reason.to_string(), look)).collect(), ..options() }
}

#[tokio::test]
async fn away_dims_or_turns_off_and_back_restores_the_plan() {
    let fake = FakeBulb::new();
    let service = attached(&fake, away_options()).await;
    ask(&service, json!({"cmd": "color", "color": "#ff0064", "brightness": 0.5})).await;

    assert_eq!(ask(&service, json!({"cmd": "away", "reason": "lock"})).await["ok"], true);
    let red = fake.last_light().r;
    assert!(0 < red && red < 40, "dimmed, not off: {red}");
    assert_eq!(ask(&service, json!({"cmd": "status"})).await["away"], "lock");

    ask(&service, json!({"cmd": "away", "reason": "sleep"})).await; // a later reason replaces the earlier one
    assert_eq!(fake.with(|s| s.channels), [0; 5]);

    assert_eq!(ask(&service, json!({"cmd": "back"})).await["ok"], true);
    let last = fake.last_light();
    assert_eq!((last.r, last.b), (128, 50)); // the plan, as it was
    let status = ask(&service, json!({"cmd": "status"})).await;
    assert_eq!(
        (status["away"].clone(), status["color"].clone(), status["on"].clone()),
        (Value::Null, json!("#ff0064"), json!(true))
    );
    service.close().await;
}

#[tokio::test]
async fn away_stops_the_running_effect_and_back_resumes_it() {
    let fake = FakeBulb::new();
    let service = attached(&fake, away_options()).await;
    ask(&service, json!({"cmd": "effect", "name": "hue"})).await;
    assert_eq!(service.state()["playing"], true);
    ask(&service, json!({"cmd": "away", "reason": "lock"})).await;
    assert_eq!(service.state()["playing"], false);
    ask(&service, json!({"cmd": "back"})).await;
    assert_eq!(service.state()["playing"], true);
    service.close().await;
}

#[tokio::test]
async fn a_request_ends_away_and_odd_away_requests_do_nothing_harmful() {
    let fake = FakeBulb::new();
    let service = attached(&fake, away_options()).await;
    ask(&service, json!({"cmd": "color", "color": "#00ff00"})).await;
    ask(&service, json!({"cmd": "away", "reason": "lock"})).await;
    ask(&service, json!({"cmd": "color", "color": "#0000ff"})).await; // somebody uses the light
    assert_eq!(ask(&service, json!({"cmd": "status"})).await["away"], Value::Null);
    assert_eq!(fake.last_light().b, 255);

    let written = fake.sent();
    assert_eq!(ask(&service, json!({"cmd": "back"})).await["ok"], true); // nothing to come back from
    assert_eq!(fake.sent(), written);

    assert_eq!(ask(&service, json!({"cmd": "away", "reason": "shutdown"})).await["ok"], true); // no look: ignored
    assert_eq!(ask(&service, json!({"cmd": "status"})).await["away"], Value::Null);
    let bad = ask(&service, json!({"cmd": "away", "reason": "boredom"})).await;
    assert_eq!(bad["ok"], false);
    assert!(bad["error"].as_str().unwrap().contains("reason"));
    service.close().await;
}

#[test]
fn presence_becomes_the_requests_of_the_python_watcher() {
    use chsmartbulb_core::service::Presence;
    assert_eq!(Presence::Lock.request(), json!({"cmd": "away", "reason": "lock"}));
    assert_eq!(Presence::Shutdown.request(), json!({"cmd": "away", "reason": "shutdown"}));
    assert_eq!(Presence::Unlock.request(), json!({"cmd": "back"}));
    assert_eq!(Presence::Resume.request(), json!({"cmd": "back"}));
}

#[tokio::test]
async fn the_clock_is_set_as_the_vendor_app_sets_it() {
    use chsmartbulb_core::protocol::ClockTime;

    let fake = FakeBulb::new();
    let bulb = Bulb::new(fake.connector());
    bulb.connect().await.unwrap();
    bulb.sync_clock(ClockTime { year: 2026, month: 10, day: 10, hour: 18, minute: 5, second: 9 }).await.unwrap();
    bulb.disconnect().await;
    assert_eq!(fake.with(|s| s.clock.clone()).unwrap(), [0, 0, 0, 0, 0, 0, 0, 0x80, 0xEA, 0x07, 10, 10, 18, 5, 9, 0]);
}
