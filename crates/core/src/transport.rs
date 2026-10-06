//! Byte pipes a bulb can be reached through.
//!
//! The core knows nothing about Bluetooth stacks: a platform layer (Android's
//! Kotlin side, BlueZ, WinRT, a test fake) implements [`Connector`] and [`Link`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, Mutex};

use crate::effects::{BLE_FPS, DEFAULT_FPS};
use crate::error::{Error, Result};

/// Which Bluetooth bearer a link uses. The bulb accepts only one at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Bearer {
    /// GATT: write with response to 0x8877, answers as notifications on 0x8888.
    Ble,
    /// Bluetooth Classic serial port (RFCOMM channel 2). Works while the bulb plays audio.
    Spp,
}

impl Bearer {
    /// Colour frames per second the bearer carries without falling behind.
    pub fn fps(self) -> f64 {
        match self {
            Bearer::Ble => BLE_FPS,
            Bearer::Spp => DEFAULT_FPS,
        }
    }
}

/// Opens links to one bulb.
#[async_trait]
pub trait Connector: Send + Sync {
    async fn connect(&self) -> Result<Arc<dyn Link>>;

    /// Which bulb and bearer, for logs and the interface.
    fn describe(&self) -> String;

    fn bearer(&self) -> Bearer;
}

/// An open, ordered, reliable byte pipe to the bulb. Reads and writes may run at the same time.
#[async_trait]
pub trait Link: Send + Sync {
    fn is_open(&self) -> bool;

    /// Send bytes; fails with [`Error::Transport`] when the link is gone.
    async fn write(&self, data: &[u8]) -> Result<()>;

    /// The next chunk of received bytes (any size, never empty); fails when the link closes.
    async fn read(&self) -> Result<Vec<u8>>;

    /// Disconnect; safe to call when already closed.
    async fn close(&self);
}

/// Writes go wherever the platform layer sends them.
#[async_trait]
pub trait Writer: Send + Sync {
    async fn write(&self, data: &[u8]) -> Result<()>;
    async fn close(&self);
}

/// A [`Link`] fed through a channel, for platform layers whose stack delivers data by callback
/// (BLE notifications, a Kotlin plugin).
pub struct ChannelLink {
    open: Arc<AtomicBool>,
    incoming: Mutex<mpsc::UnboundedReceiver<Vec<u8>>>,
    wake: mpsc::UnboundedSender<Vec<u8>>,
    writer: Box<dyn Writer>,
}

/// The side of a [`ChannelLink`] the platform layer pushes received bytes into.
#[derive(Clone)]
pub struct LinkFeed {
    open: Arc<AtomicBool>,
    sender: mpsc::UnboundedSender<Vec<u8>>,
}

impl LinkFeed {
    pub fn push(&self, data: Vec<u8>) {
        if !data.is_empty() {
            let _ = self.sender.send(data);
        }
    }

    /// The link went down on the platform's side.
    pub fn closed(&self) {
        self.open.store(false, Ordering::SeqCst);
        let _ = self.sender.send(Vec::new()); // wakes a pending read
    }
}

impl ChannelLink {
    pub fn new(writer: Box<dyn Writer>) -> (Arc<Self>, LinkFeed) {
        Self::new_with(|_| writer)
    }

    /// Like [`new`](Self::new), for a writer that answers through the feed itself.
    pub fn new_with(writer: impl FnOnce(LinkFeed) -> Box<dyn Writer>) -> (Arc<Self>, LinkFeed) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let open = Arc::new(AtomicBool::new(true));
        let feed = LinkFeed { open: open.clone(), sender: sender.clone() };
        let writer = writer(feed.clone());
        let link = Arc::new(Self { open, incoming: Mutex::new(receiver), wake: sender, writer });
        (link, feed)
    }
}

#[async_trait]
impl Link for ChannelLink {
    fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }

    async fn write(&self, data: &[u8]) -> Result<()> {
        if !self.is_open() {
            return Err(Error::Transport("the link is closed".into()));
        }
        let result = self.writer.write(data).await;
        if result.is_err() {
            self.open.store(false, Ordering::SeqCst);
        }
        result
    }

    async fn read(&self) -> Result<Vec<u8>> {
        let mut incoming = self.incoming.lock().await;
        loop {
            if !self.is_open() {
                return Err(Error::Transport("the link was closed".into()));
            }
            match incoming.recv().await {
                Some(data) if !data.is_empty() => return Ok(data),
                Some(_) => continue, // a wake-up from closed(); the flag says what happened
                None => {
                    self.open.store(false, Ordering::SeqCst);
                    return Err(Error::Transport("the link was closed".into()));
                }
            }
        }
    }

    async fn close(&self) {
        if self.open.swap(false, Ordering::SeqCst) {
            let _ = self.wake.send(Vec::new()); // ends a pending read
            self.writer.close().await;
        }
    }
}
