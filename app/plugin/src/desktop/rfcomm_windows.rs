//! Bluetooth Classic serial port (SPP over RFCOMM) through a Winsock Bluetooth socket.
//!
//! Winsock sockets block, so reads run on a thread of their own and feed a
//! [`ChannelLink`]; writes run on the blocking pool.

use std::sync::Arc;

use async_trait::async_trait;
use chsmartbulb_core::protocol::RFCOMM_CHANNEL;
use chsmartbulb_core::transport::{ChannelLink, Writer};
use chsmartbulb_core::{Bearer, Connector, Error, Link, Result};
use windows_sys::core::GUID;
use windows_sys::Win32::Devices::Bluetooth::{AF_BTH, BTHPROTO_RFCOMM, SOCKADDR_BTH};
use windows_sys::Win32::Networking::WinSock::{
    closesocket, connect, recv, send, shutdown, socket, WSACleanup, WSAGetLastError, WSAStartup, INVALID_SOCKET,
    SD_BOTH, SOCKADDR, SOCKET, SOCKET_ERROR, SOCK_STREAM, WSADATA,
};

pub struct RfcommConnector {
    address: String,
    bt_addr: u64,
}

impl RfcommConnector {
    pub fn new(address: &str) -> crate::Result<Self> {
        let digits: String = address.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        let bt_addr = (digits.len() == 12)
            .then(|| u64::from_str_radix(&digits, 16).ok())
            .flatten()
            .ok_or_else(|| crate::Error::Bluetooth(format!("not a Bluetooth address: {address}")))?;
        Ok(Self { address: address.to_ascii_uppercase(), bt_addr })
    }
}

/// A connected socket; closed once both the reader and the writer are done with it.
struct Socket(SOCKET);

impl Drop for Socket {
    fn drop(&mut self) {
        // SAFETY: the socket is ours and nothing uses it any more.
        unsafe {
            closesocket(self.0);
            WSACleanup();
        }
    }
}

fn last_error() -> String {
    // SAFETY: reads the calling thread's last Winsock error.
    format!("winsock error {}", unsafe { WSAGetLastError() })
}

fn open(bt_addr: u64) -> std::result::Result<Socket, String> {
    // SAFETY: plain Winsock calls with valid, owned arguments.
    unsafe {
        let mut data: WSADATA = std::mem::zeroed();
        if WSAStartup(0x0202, &mut data) != 0 {
            return Err("cannot start Winsock".into());
        }
        let raw = socket(AF_BTH as i32, SOCK_STREAM, BTHPROTO_RFCOMM as i32);
        if raw == INVALID_SOCKET {
            let error = last_error();
            WSACleanup();
            return Err(error);
        }
        let socket = Socket(raw);
        let mut address: SOCKADDR_BTH = std::mem::zeroed();
        address.addressFamily = AF_BTH;
        address.btAddr = bt_addr;
        address.serviceClassId = GUID::from_u128(0);
        address.port = u32::from(RFCOMM_CHANNEL);
        let length = std::mem::size_of::<SOCKADDR_BTH>() as i32;
        if connect(socket.0, (&address as *const SOCKADDR_BTH).cast::<SOCKADDR>(), length) == SOCKET_ERROR {
            return Err(last_error());
        }
        Ok(socket)
    }
}

struct SocketWriter {
    socket: Arc<Socket>,
}

#[async_trait]
impl Writer for SocketWriter {
    async fn write(&self, data: &[u8]) -> Result<()> {
        let socket = self.socket.clone();
        let data = data.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut sent = 0;
            while sent < data.len() {
                let rest = &data[sent..];
                // SAFETY: sends from a valid buffer on our connected socket.
                let result = unsafe { send(socket.0, rest.as_ptr(), rest.len() as i32, 0) };
                if result == SOCKET_ERROR {
                    return Err(Error::Transport(format!("RFCOMM write failed: {}", last_error())));
                }
                sent += result as usize;
            }
            Ok(())
        })
        .await
        .map_err(|e| Error::Transport(e.to_string()))?
    }

    async fn close(&self) {
        // SAFETY: shutting the socket down wakes the reader thread, which then lets go of it.
        unsafe { shutdown(self.socket.0, SD_BOTH) };
    }
}

#[async_trait]
impl Connector for RfcommConnector {
    async fn connect(&self) -> Result<Arc<dyn Link>> {
        let bt_addr = self.bt_addr;
        let target = format!("{} channel {RFCOMM_CHANNEL}", self.address);
        let socket = tokio::task::spawn_blocking(move || open(bt_addr))
            .await
            .map_err(|e| Error::ConnectionFailed(e.to_string()))?
            .map_err(|e| Error::ConnectionFailed(format!("cannot connect to {target}: {e}")))?;
        let socket = Arc::new(socket);
        let (link, feed) = ChannelLink::new(Box::new(SocketWriter { socket: socket.clone() }));
        std::thread::spawn(move || {
            let mut buffer = [0u8; 1024];
            loop {
                // SAFETY: reads into a buffer we own, at most its length.
                let size = unsafe { recv(socket.0, buffer.as_mut_ptr(), buffer.len() as i32, 0) };
                if size <= 0 {
                    feed.closed();
                    return;
                }
                feed.push(buffer[..size as usize].to_vec());
            }
        });
        Ok(link)
    }

    fn describe(&self) -> String {
        format!("{} over SPP", self.address)
    }

    fn bearer(&self) -> Bearer {
        Bearer::Spp
    }
}
