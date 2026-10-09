//! BLE through btleplug: write with response to 0x8877, answers as notifications on 0x8888.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use btleplug::api::{Central, Characteristic, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::Peripheral;
use chsmartbulb_core::protocol::{BLE_NOTIFY_UUID, BLE_WRITE_UUID};
use chsmartbulb_core::transport::{ChannelLink, Writer};
use chsmartbulb_core::{Bearer, Connector, Error, Link, Result};
use futures::StreamExt;

const FIND_TIMEOUT: Duration = Duration::from_secs(15);

pub struct BleConnector {
    address: String,
}

impl BleConnector {
    pub fn new(address: &str) -> Self {
        Self { address: address.to_ascii_uppercase() }
    }

    async fn find(&self) -> Result<Peripheral> {
        let adapter = crate::adapter().await.map_err(chsmartbulb_core::Error::from)?;
        let deadline = tokio::time::Instant::now() + FIND_TIMEOUT;
        let mut scanning = false;
        let found = loop {
            let peripherals = adapter.peripherals().await.map_err(failed)?;
            if let Some(found) = peripherals.into_iter().find(|p| p.address().to_string() == self.address) {
                break Some(found);
            }
            if tokio::time::Instant::now() >= deadline {
                break None;
            }
            if !scanning {
                adapter.start_scan(ScanFilter::default()).await.map_err(failed)?;
                scanning = true;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        };
        if scanning {
            let _ = adapter.stop_scan().await;
        }
        found.ok_or_else(|| {
            Error::ConnectionFailed(format!(
                "{} is not advertising over BLE; while it is connected over Classic (as a speaker) it does not",
                self.address
            ))
        })
    }
}

fn failed(error: btleplug::Error) -> Error {
    Error::ConnectionFailed(error.to_string())
}

fn characteristic(peripheral: &Peripheral, uuid: &str) -> Result<Characteristic> {
    peripheral.characteristics().into_iter().find(|c| c.uuid.to_string() == uuid).ok_or_else(|| {
        // BlueZ connects a bulb paired as a speaker over Classic whatever was asked for, and
        // its services are then not the bulb's (see docs/device.md, "BLE on Linux / BlueZ")
        let hint = if cfg!(target_os = "linux") {
            "the system connected it as a speaker (Bluetooth Classic) instead of over BLE; use the rfcomm transport"
        } else {
            "is it the bulb?"
        };
        Error::ConnectionFailed(format!("the device has no characteristic {uuid}: {hint}"))
    })
}

struct BleWriter {
    peripheral: Peripheral,
    command: Characteristic,
}

#[async_trait]
impl Writer for BleWriter {
    async fn write(&self, data: &[u8]) -> Result<()> {
        // The bulb ignores write-without-response, so always ask for one.
        self.peripheral
            .write(&self.command, data, WriteType::WithResponse)
            .await
            .map_err(|e| Error::Transport(format!("BLE write failed: {e}")))
    }

    async fn close(&self) {
        let _ = self.peripheral.disconnect().await;
    }
}

#[async_trait]
impl Connector for BleConnector {
    async fn connect(&self) -> Result<Arc<dyn Link>> {
        let peripheral = self.find().await?;
        let opened = async {
            if !peripheral.is_connected().await.map_err(failed)? {
                peripheral.connect().await.map_err(failed)?;
            }
            peripheral.discover_services().await.map_err(failed)?;
            let command = characteristic(&peripheral, BLE_WRITE_UUID)?;
            let answers = characteristic(&peripheral, BLE_NOTIFY_UUID)?;
            // BlueZ reports "connected" even when it brought up the Classic link of a
            // dual-mode device instead of LE; a GATT read exposes that right away.
            peripheral.read(&command).await.map_err(failed)?;
            peripheral.subscribe(&answers).await.map_err(failed)?;
            let notifications = peripheral.notifications().await.map_err(failed)?;
            Ok::<_, Error>((command, notifications))
        };
        let (command, mut notifications) = match opened.await {
            Ok(opened) => opened,
            Err(error) => {
                let _ = peripheral.disconnect().await;
                return Err(Error::ConnectionFailed(format!("cannot connect to {} over BLE: {error}", self.address)));
            }
        };
        let (link, feed) = ChannelLink::new(Box::new(BleWriter { peripheral: peripheral.clone(), command }));
        let answers = feed.clone();
        tokio::spawn(async move {
            while let Some(notification) = notifications.next().await {
                if notification.uuid.to_string() == BLE_NOTIFY_UUID {
                    answers.push(notification.value);
                }
            }
            answers.closed();
        });
        let watched = Arc::downgrade(&link);
        tokio::spawn(async move {
            // not every backend ends the notification stream when the link drops
            while let Some(link) = watched.upgrade() {
                if !link.is_open() {
                    return;
                }
                drop(link);
                if !peripheral.is_connected().await.unwrap_or(false) {
                    feed.closed();
                    return;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
        Ok(link)
    }

    fn describe(&self) -> String {
        format!("{} over BLE", self.address)
    }

    fn bearer(&self) -> Bearer {
        Bearer::Ble
    }
}
