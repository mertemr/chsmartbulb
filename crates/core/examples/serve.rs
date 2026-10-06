//! The Rust service on the network with a simulated bulb, for trying the web interface,
//! the command line and the agents without hardware:
//!
//! ```bash
//! cargo run -p chsmartbulb-core --features server --example serve
//! chsmartbulb --host 127.0.0.1 status        # and http://127.0.0.1:8378
//! ```

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use chsmartbulb_core::server::{self, Assets, LINES_PORT, WEB_PORT};
use chsmartbulb_core::sim::SimulatedBulb;
use chsmartbulb_core::{Bulb, Service};

fn load(root: &Path, dir: &Path, assets: &mut Assets) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            load(root, &path, assets)?;
        } else {
            let name = path.strip_prefix(root).expect("under root").to_string_lossy().replace('\\', "/");
            assets.insert(&name, std::fs::read(&path)?);
        }
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src/chsmartbulb/webui");
    let mut assets = Assets::default();
    load(&root, &root, &mut assets)?;

    let service = Service::builder(Bulb::new(SimulatedBulb::new().connector())).build();
    service.start();
    let any = |port| SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let lines = server::bind(any(LINES_PORT)).await?;
    let web = server::bind(any(WEB_PORT)).await?;
    println!("line protocol on 127.0.0.1:{LINES_PORT}, web interface on http://127.0.0.1:{WEB_PORT}");
    tokio::join!(
        server::serve_lines(service.clone(), lines, None),
        server::serve_web(service.clone(), web, Arc::new(assets), None),
    );
    Ok(())
}
