//! The background service, as `chsmartbulb daemon` of the Python package runs it: it keeps
//! the link to the bulb, runs the effects and answers the socket protocol on a local
//! socket and, when asked, on the network together with the web interface.
//!
//! ```bash
//! chsmartbulbd --address AA:BB:CC:DD:EE:FF --listen 8377 --web 8378 --token SECRET
//! chsmartbulbd --simulate --web 8378 --no-token      # no bulb needed
//! ```

use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;

use chsmartbulb_core::effects::DEFAULT_FPS;
use chsmartbulb_core::protocol::RFCOMM_CHANNEL;
use chsmartbulb_core::screen::PRIMARY;
use chsmartbulb_core::server::{self, Assets, LINES_PORT, WEB_PORT};
use chsmartbulb_core::service::{AwayLook, Options, Presence};
use chsmartbulb_core::sim::SimulatedBulb;
use chsmartbulb_core::{Bearer, Bulb, Connector, Service};
use clap::{Parser, ValueEnum};
use include_dir::{include_dir, Dir};

/// The page the Python package carries, so the two serve the same one.
static WEB_INTERFACE: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../web/bundle");

#[derive(Clone, Copy, ValueEnum)]
enum Transport {
    Rfcomm,
    Ble,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum Look {
    None,
    Dim,
    Off,
}

impl Look {
    fn away(self) -> Option<AwayLook> {
        match self {
            Look::None => None,
            Look::Dim => Some(AwayLook::Dim),
            Look::Off => Some(AwayLook::Off),
        }
    }
}

/// Run the background service of a CHSmartBulb / BL08A bulb in the foreground.
#[derive(Parser)]
#[command(name = "chsmartbulbd", version)]
struct Args {
    /// Bluetooth address of the bulb
    #[arg(short, long, env = "CHSMARTBULB_ADDRESS", required_unless_present = "simulate")]
    address: Option<String>,
    #[arg(short, long, value_enum, default_value = "rfcomm")]
    transport: Transport,
    /// RFCOMM channel
    #[arg(long, default_value_t = RFCOMM_CHANNEL)]
    channel: u8,
    /// Serve a simulated bulb instead of a real one, for trying things out
    #[arg(long)]
    simulate: bool,
    /// Service socket path
    #[cfg(unix)]
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Also accept other machines on this address (needs a token)
    #[arg(long, value_name = "[HOST:]PORT", value_parser = listen_address, help = format!("Also accept other machines on this address (needs a token; usual port {LINES_PORT})"))]
    listen: Option<SocketAddr>,
    /// Also serve the web interface on this address (needs a token)
    #[arg(long, value_name = "[HOST:]PORT", value_parser = listen_address, help = format!("Also serve the web interface on this address (needs a token; usual port {WEB_PORT})"))]
    web: Option<SocketAddr>,
    /// Shared secret for network access
    #[arg(long, env = "CHSMARTBULB_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Let anyone who can reach --listen and --web use them, without a token
    #[arg(long)]
    no_token: bool,
    /// Effect frame rate
    #[arg(long, default_value_t = DEFAULT_FPS)]
    fps: f64,
    /// Do not remember the light state across restarts
    #[arg(long)]
    no_state: bool,
    /// Audio source for sound-reactive effects (default: monitor of the default output; Linux)
    #[arg(long, value_name = "SOURCE")]
    audio_device: Option<String>,
    /// Listen to the microphone instead of what the computer plays
    #[arg(long)]
    mic: bool,
    /// Which monitor of this computer the screen effect follows; 0 is all (a Wayland desktop asks instead)
    #[arg(long, value_name = "N", default_value_t = PRIMARY)]
    monitor: i64,
    /// Forget the screen a Wayland desktop was told to share, so it asks again
    #[arg(long)]
    choose_screen: bool,
    /// What the light does while this computer's screen is locked, until it is unlocked
    #[arg(long, value_enum, default_value = "none")]
    on_lock: Look,
    /// What the light does while this computer sleeps or shuts down, until it is back
    #[arg(long, value_enum, default_value = "none")]
    on_sleep: Look,
    #[arg(short, long)]
    verbose: bool,
}

fn listen_address(text: &str) -> Result<SocketAddr, String> {
    let (host, port) = text.rsplit_once(':').unwrap_or(("0.0.0.0", text));
    let port: u16 = port.parse().map_err(|_| "expected PORT or HOST:PORT".to_string())?;
    let mut found = (host, port).to_socket_addrs().map_err(|error| format!("{host}: {error}"))?;
    found.next().ok_or_else(|| format!("{host}: no such address"))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

/// Where the Python service keeps the light state too, so either picks up where the other left.
fn default_state_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).or_else(|| Some(home()?.join(".local/state")))?;
    Some(base.join("chsmartbulb/state.json"))
}

/// Where the Python command line looks for the service.
#[cfg(unix)]
fn default_socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| {
        // SAFETY: getuid cannot fail and touches no memory.
        let user = unsafe { libc::getuid() };
        std::env::temp_dir().join(format!("chsmartbulb-{user}"))
    });
    base.join("chsmartbulb.sock")
}

/// Removes the socket when the service ends.
#[cfg(unix)]
struct LocalSocket(PathBuf);

#[cfg(unix)]
impl Drop for LocalSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(unix)]
async fn bind_local(path: PathBuf) -> Result<(tokio::net::UnixListener, LocalSocket), String> {
    use std::os::unix::fs::PermissionsExt;

    if tokio::net::UnixStream::connect(&path).await.is_ok() {
        return Err(format!("a service is already listening on {}", path.display()));
    }
    let failed = |error: std::io::Error| format!("cannot listen on {}: {error}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(failed)?;
    }
    let _ = std::fs::remove_file(&path); // left behind by a service that was killed
    let listener = tokio::net::UnixListener::bind(&path).map_err(failed)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(failed)?;
    Ok((listener, LocalSocket(path)))
}

fn web_interface() -> Assets {
    fn load(dir: &Dir<'_>, assets: &mut Assets) {
        for file in dir.files() {
            assets.insert(&file.path().to_string_lossy().replace('\\', "/"), file.contents().to_vec());
        }
        for dir in dir.dirs() {
            load(dir, assets);
        }
    }
    let mut assets = Assets::default();
    load(&WEB_INTERFACE, &mut assets);
    assets
}

fn connector(args: &Args) -> Result<Arc<dyn Connector>, String> {
    if args.simulate {
        let simulated = SimulatedBulb::new();
        simulated.with(|bulb| bulb.channels = [0, 0, 0, 255, 0]); // lit white, as a bulb usually comes on
        return Ok(simulated.connector());
    }
    let address = args.address.as_deref().expect("required without --simulate");
    let connector = match args.transport {
        Transport::Rfcomm => chsmartbulb_desktop::serial_port(address, args.channel),
        Transport::Ble => chsmartbulb_desktop::connector(address, Bearer::Ble),
    };
    connector.map_err(|error| error.to_string())
}

fn away_looks(args: &Args) -> HashMap<String, AwayLook> {
    let mut looks = HashMap::new();
    if let Some(look) = args.on_lock.away() {
        looks.insert("lock".to_string(), look);
    }
    if let Some(look) = args.on_sleep.away() {
        looks.insert("sleep".to_string(), look);
        looks.insert("shutdown".to_string(), look);
    }
    looks
}

async fn interrupted() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut terminate) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

async fn run(args: Args) -> Result<(), String> {
    let network = args.listen.is_some() || args.web.is_some();
    let token = if args.no_token { None } else { args.token.clone().filter(|token| !token.is_empty()) };
    if network && token.is_none() && !args.no_token {
        return Err(
            "--listen and --web need a token: use --token, set $CHSMARTBULB_TOKEN or pass --no-token".to_string()
        );
    }
    #[cfg(not(unix))]
    if !network {
        return Err("this system has no local socket: give --listen and/or --web".to_string());
    }

    let looks = away_looks(&args);
    let options = Options {
        state_path: if args.no_state { None } else { default_state_path() },
        fps: args.fps,
        away_looks: looks.clone(),
        monitor: args.monitor,
        ..Options::default()
    };
    let mut builder = Service::builder(Bulb::new(connector(&args)?)).options(options);
    if let Some(capture) = chsmartbulb_desktop::audio_capture(args.mic, args.audio_device.clone()) {
        builder = builder.audio_capture(capture);
    }
    let shared_screen = default_state_path().map(|state| state.with_file_name("shared-screen"));
    if args.choose_screen {
        if let Some(path) = &shared_screen {
            let _ = std::fs::remove_file(path);
        }
    }
    if let Some(capture) = chsmartbulb_desktop::screen_capture(shared_screen) {
        builder = builder.screen_capture(capture);
    }
    let service = builder.build();

    // everything is bound before the bulb is touched, so a taken port costs nothing
    #[cfg(unix)]
    let (local, _socket) = bind_local(args.socket.clone().unwrap_or_else(default_socket_path)).await?;
    let bind = |address: SocketAddr| async move {
        server::bind(address).await.map_err(|error| format!("cannot listen on {address}: {error}"))
    };
    let lines = match args.listen {
        Some(address) => Some(bind(address).await?),
        None => None,
    };
    let web = match args.web {
        Some(address) => Some(bind(address).await?),
        None => None,
    };

    let mut serving = Vec::new();
    #[cfg(unix)]
    serving.push(tokio::spawn(server::serve_local(service.clone(), local)));
    if let Some(listener) = lines {
        log::info!("listening on {}", listener.local_addr().map_err(|e| e.to_string())?);
        serving.push(tokio::spawn(server::serve_lines(service.clone(), listener, token.clone())));
    }
    if let Some(listener) = web {
        log::info!("web interface on http://{}", listener.local_addr().map_err(|e| e.to_string())?);
        let assets = Arc::new(web_interface());
        serving.push(tokio::spawn(server::serve_web(service.clone(), listener, assets, token.clone())));
    }
    if network && token.is_none() {
        log::warn!("no token is asked for: anyone who can reach this machine controls the light");
    }

    service.start();
    let _presence = if looks.is_empty() {
        None
    } else {
        let told = service.clone();
        let on_event = Arc::new(move |event: Presence| {
            let service = told.clone();
            tokio::spawn(async move {
                service.handle(&event.request()).await;
            });
        });
        match chsmartbulb_desktop::presence::watch(on_event).await {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                log::warn!("cannot follow this computer being locked or asleep: {error}");
                None
            }
        }
    };

    interrupted().await;
    for task in serving {
        task.abort();
    }
    service.close().await;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let args = Args::parse();
    let level = if args.verbose { log::LevelFilter::Debug } else { log::LevelFilter::Info };
    env_logger::Builder::new().filter_level(level).parse_default_env().init();
    match run(args).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("chsmartbulbd: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
