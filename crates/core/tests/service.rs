//! The background service against a fake bulb; mirrors `tests/test_service.py`.

mod fake;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chsmartbulb_core::audio::AudioSource;
use chsmartbulb_core::catalog::Needs;
use chsmartbulb_core::service::{AudioCapture, Options, Service, Session};
use chsmartbulb_core::{Bulb, Result};
use fake::FakeBulb;
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
}

#[async_trait]
impl AudioCapture for FakeCapture {
    async fn start(&self, _source: Arc<AudioSource>) -> Result<()> {
        self.running.store(true, Ordering::SeqCst);
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
    ask(&service, json!({"cmd": "color", "color": "red"})).await;
    assert!(!capture.running.load(Ordering::SeqCst));
    // an agent takes over from the local capture
    ask(&service, json!({"cmd": "effect", "name": "music"})).await;
    service.agent_joined(Needs::Audio).await;
    assert!(!capture.running.load(Ordering::SeqCst));
    assert_eq!(service.state()["audio"], "agent");
    service.agent_left(Needs::Audio).await;
    assert!(capture.running.load(Ordering::SeqCst));
    service.close().await;
    assert!(!capture.running.load(Ordering::SeqCst));
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
    assert_eq!(status["problem"], "fake: host is down");
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
    assert_eq!(reply["effects"].as_array().unwrap().len(), 13);
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
