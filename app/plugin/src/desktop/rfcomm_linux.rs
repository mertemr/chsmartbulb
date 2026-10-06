//! Bluetooth Classic serial port (SPP over RFCOMM) through a BlueZ socket.
//!
//! The bulb must already be paired. Reopening the channel right after closing it
//! fails with `EBUSY` for about half a second, so connecting retries that case.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chsmartbulb_core::protocol::RFCOMM_CHANNEL;
use chsmartbulb_core::{Bearer, Connector, Error, Link, Result};
use tokio::io::unix::AsyncFd;
use tokio::io::Interest;

const AF_BLUETOOTH: libc::c_int = 31;
const BTPROTO_RFCOMM: libc::c_int = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const BUSY_RETRIES: u32 = 6;

#[repr(C)]
struct SockaddrRc {
    family: libc::sa_family_t,
    bdaddr: [u8; 6],
    channel: u8,
}

pub struct RfcommConnector {
    address: String,
    bdaddr: [u8; 6],
}

impl RfcommConnector {
    pub fn new(address: &str) -> crate::Result<Self> {
        let parts: Vec<u8> = address.split(':').filter_map(|part| u8::from_str_radix(part, 16).ok()).collect();
        let bytes: [u8; 6] =
            parts.try_into().map_err(|_| crate::Error::Bluetooth(format!("not a Bluetooth address: {address}")))?;
        let mut bdaddr = bytes;
        bdaddr.reverse(); // BlueZ keeps addresses least significant byte first
        Ok(Self { address: address.to_ascii_uppercase(), bdaddr })
    }

    async fn attempt(&self) -> io::Result<OwnedFd> {
        // SAFETY: plain socket creation; the descriptor is owned right away.
        let raw = unsafe {
            libc::socket(AF_BLUETOOTH, libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, BTPROTO_RFCOMM)
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a fresh descriptor nobody else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let address =
            SockaddrRc { family: AF_BLUETOOTH as libc::sa_family_t, bdaddr: self.bdaddr, channel: RFCOMM_CHANNEL };
        // SAFETY: the address struct matches `struct sockaddr_rc` and outlives the call.
        let started = unsafe {
            libc::connect(
                fd.as_raw_fd(),
                (&address as *const SockaddrRc).cast(),
                std::mem::size_of::<SockaddrRc>() as libc::socklen_t,
            )
        };
        if started < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINPROGRESS) {
                return Err(error);
            }
        }
        let fd = AsyncFd::with_interest(fd, Interest::WRITABLE)?;
        tokio::time::timeout(CONNECT_TIMEOUT, fd.writable())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "timed out"))??
            .retain_ready();
        let mut status: libc::c_int = 0;
        let mut length = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: SO_ERROR writes one int into `status`.
        let read = unsafe {
            libc::getsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&mut status as *mut libc::c_int).cast(),
                &mut length,
            )
        };
        if read < 0 {
            return Err(io::Error::last_os_error());
        }
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status));
        }
        Ok(fd.into_inner())
    }
}

#[async_trait]
impl Connector for RfcommConnector {
    async fn connect(&self) -> Result<Arc<dyn Link>> {
        let target = format!("{} channel {RFCOMM_CHANNEL}", self.address);
        for attempt in 0..=BUSY_RETRIES {
            match self.attempt().await {
                Ok(fd) => {
                    let fd = AsyncFd::with_interest(fd, Interest::READABLE | Interest::WRITABLE)
                        .map_err(|e| Error::ConnectionFailed(e.to_string()))?;
                    return Ok(Arc::new(RfcommLink { fd, open: AtomicBool::new(true) }));
                }
                Err(error) if error.raw_os_error() == Some(libc::EBUSY) && attempt < BUSY_RETRIES => {
                    tokio::time::sleep(Duration::from_millis(400 * u64::from(attempt + 1))).await;
                }
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                    return Err(Error::ConnectionFailed(format!("timed out connecting to {target}")));
                }
                Err(error) => return Err(Error::ConnectionFailed(format!("cannot connect to {target}: {error}"))),
            }
        }
        Err(Error::ConnectionFailed(format!("{target} stayed busy")))
    }

    fn describe(&self) -> String {
        format!("{} over SPP", self.address)
    }

    fn bearer(&self) -> Bearer {
        Bearer::Spp
    }
}

struct RfcommLink {
    fd: AsyncFd<OwnedFd>,
    open: AtomicBool,
}

impl RfcommLink {
    fn lost(&self, what: &str, error: impl std::fmt::Display) -> Error {
        self.open.store(false, Ordering::SeqCst);
        Error::Transport(format!("RFCOMM {what} failed: {error}"))
    }
}

#[async_trait]
impl Link for RfcommLink {
    fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }

    async fn write(&self, data: &[u8]) -> Result<()> {
        let mut sent = 0;
        while sent < data.len() {
            if !self.is_open() {
                return Err(Error::Transport("RFCOMM channel is closed".into()));
            }
            let mut ready = self.fd.writable().await.map_err(|e| self.lost("write", e))?;
            let rest = &data[sent..];
            // SAFETY: writes from a valid buffer to our own descriptor.
            let result =
                ready.try_io(|fd| cvt(unsafe { libc::write(fd.as_raw_fd(), rest.as_ptr().cast(), rest.len()) }));
            match result {
                Ok(Ok(written)) => sent += written,
                Ok(Err(error)) => return Err(self.lost("write", error)),
                Err(_would_block) => continue,
            }
        }
        Ok(())
    }

    async fn read(&self) -> Result<Vec<u8>> {
        let mut buffer = vec![0u8; 1024];
        loop {
            if !self.is_open() {
                return Err(Error::Transport("RFCOMM channel is closed".into()));
            }
            let mut ready = self.fd.readable().await.map_err(|e| self.lost("read", e))?;
            // SAFETY: reads into a buffer we own, at most its length.
            let result =
                ready.try_io(|fd| cvt(unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) }));
            match result {
                Ok(Ok(0)) => return Err(self.lost("read", "closed by the device")),
                Ok(Ok(size)) => {
                    buffer.truncate(size);
                    return Ok(buffer);
                }
                Ok(Err(error)) => return Err(self.lost("read", error)),
                Err(_would_block) => continue,
            }
        }
    }

    async fn close(&self) {
        if self.open.swap(false, Ordering::SeqCst) {
            // SAFETY: shutting down our own descriptor wakes a pending read; it is closed on drop.
            unsafe { libc::shutdown(self.fd.as_raw_fd(), libc::SHUT_RDWR) };
        }
    }
}

fn cvt(result: isize) -> io::Result<usize> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result as usize)
    }
}
