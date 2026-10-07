//! The computer being locked, put to sleep or shut down, as events, like `chsmartbulb.presence`.
//!
//! Linux follows logind on the system bus: the session's `LockedHint` and its `Lock` and
//! `Unlock` signals, and the manager's `PrepareForSleep` and `PrepareForShutdown`. Windows
//! follows what it tells every window: session lock and unlock, suspend and resume, and the
//! session ending, received by a hidden window on a thread of its own.

use std::sync::Arc;

use crate::models::Presence;

type Callback = Arc<dyn Fn(Presence) + Send + Sync>;

/// Keeps watching until dropped.
pub struct Watcher {
    #[cfg(target_os = "linux")]
    task: tokio::task::JoinHandle<()>,
    #[cfg(windows)]
    window: Arc<std::sync::atomic::AtomicIsize>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        self.task.abort();
        #[cfg(windows)]
        windows::close(&self.window);
    }
}

#[cfg(target_os = "linux")]
pub async fn watch(on_event: Callback) -> crate::Result<Watcher> {
    let task = linux::start(on_event).await?;
    Ok(Watcher { task })
}

#[cfg(windows)]
pub async fn watch(on_event: Callback) -> crate::Result<Watcher> {
    let window = windows::start(on_event).await?;
    Ok(Watcher { window })
}

#[cfg(not(any(target_os = "linux", windows)))]
pub async fn watch(_on_event: Callback) -> crate::Result<Watcher> {
    Err(crate::Error::Unsupported("following whether the computer is locked or asleep is not available here".into()))
}

#[cfg(target_os = "linux")]
mod linux {
    use futures::StreamExt;
    use zbus::zvariant::OwnedObjectPath;

    use super::Callback;
    use crate::models::Presence;
    use crate::{Error, Result};

    #[zbus::proxy(
        interface = "org.freedesktop.login1.Manager",
        default_service = "org.freedesktop.login1",
        default_path = "/org/freedesktop/login1"
    )]
    trait Manager {
        fn get_session(&self, session_id: &str) -> zbus::Result<OwnedObjectPath>;
        fn get_session_by_pid(&self, pid: u32) -> zbus::Result<OwnedObjectPath>;
        #[zbus(signal)]
        fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
        #[zbus(signal)]
        fn prepare_for_shutdown(&self, start: bool) -> zbus::Result<()>;
    }

    #[zbus::proxy(interface = "org.freedesktop.login1.Session", default_service = "org.freedesktop.login1")]
    trait Session {
        #[zbus(signal)]
        fn lock(&self) -> zbus::Result<()>;
        #[zbus(signal)]
        fn unlock(&self) -> zbus::Result<()>;
        #[zbus(property)]
        fn locked_hint(&self) -> zbus::Result<bool>;
    }

    fn failed(error: zbus::Error) -> Error {
        Error::Unsupported(format!("cannot follow logind: {error}"))
    }

    pub async fn start(on_event: Callback) -> Result<tokio::task::JoinHandle<()>> {
        let bus = zbus::Connection::system().await.map_err(failed)?;
        let manager = ManagerProxy::new(&bus).await.map_err(failed)?;
        let path = match manager.get_session("auto").await {
            Ok(path) => path,
            Err(_) => manager.get_session_by_pid(std::process::id()).await.map_err(failed)?,
        };
        let session = SessionProxy::builder(&bus).path(path).map_err(failed)?.build().await.map_err(failed)?;
        let mut sleeping = manager.receive_prepare_for_sleep().await.map_err(failed)?;
        let mut shutting_down = manager.receive_prepare_for_shutdown().await.map_err(failed)?;
        let mut locks = session.receive_lock().await.map_err(failed)?;
        let mut unlocks = session.receive_unlock().await.map_err(failed)?;
        let mut hints = session.receive_locked_hint_changed().await;
        Ok(tokio::spawn(async move {
            let _keep = (bus, manager, session);
            loop {
                let event = tokio::select! {
                    Some(signal) = sleeping.next() => match signal.args() {
                        Ok(args) if *args.start() => Presence::Sleep,
                        Ok(_) => Presence::Resume,
                        Err(_) => continue,
                    },
                    Some(signal) = shutting_down.next() => match signal.args() {
                        Ok(args) if *args.start() => Presence::Shutdown,
                        _ => continue,
                    },
                    Some(_) = locks.next() => Presence::Lock,
                    Some(_) = unlocks.next() => Presence::Unlock,
                    Some(change) = hints.next() => match change.get().await {
                        Ok(true) => Presence::Lock,
                        Ok(false) => Presence::Unlock,
                        Err(_) => continue,
                    },
                    else => return,
                };
                log::debug!("presence: {event:?}");
                on_event(event);
            }
        }))
    }
}

#[cfg(windows)]
mod windows {
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::{Arc, OnceLock};

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::RemoteDesktop::{WTSRegisterSessionNotification, WTSUnRegisterSessionNotification};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, PostMessageW, PostQuitMessage,
        RegisterClassW, TranslateMessage, MSG, WM_CLOSE, WM_DESTROY, WM_ENDSESSION, WM_POWERBROADCAST,
        WM_QUERYENDSESSION, WM_WTSSESSION_CHANGE, WNDCLASSW,
    };

    use super::Callback;
    use crate::models::Presence;
    use crate::{Error, Result};

    const WTS_SESSION_LOCK: usize = 7;
    const WTS_SESSION_UNLOCK: usize = 8;
    const PBT_APMSUSPEND: usize = 4;
    const PBT_APMRESUMESUSPEND: usize = 7;
    const PBT_APMRESUMEAUTOMATIC: usize = 0x12;
    const NOTIFY_FOR_THIS_SESSION: u32 = 0;

    /// The callback the window procedure reports to; one watcher per process.
    static REPORT: OnceLock<std::sync::Mutex<Option<Callback>>> = OnceLock::new();

    fn report() -> &'static std::sync::Mutex<Option<Callback>> {
        REPORT.get_or_init(|| std::sync::Mutex::new(None))
    }

    /// What a window message says about the computer, as `chsmartbulb.presence.event_of` reads it.
    pub fn event_of(message: u32, wparam: usize) -> Option<Presence> {
        match (message, wparam) {
            (WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK) => Some(Presence::Lock),
            (WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK) => Some(Presence::Unlock),
            (WM_POWERBROADCAST, PBT_APMSUSPEND) => Some(Presence::Sleep),
            (WM_POWERBROADCAST, PBT_APMRESUMESUSPEND | PBT_APMRESUMEAUTOMATIC) => Some(Presence::Resume),
            (WM_ENDSESSION, wparam) if wparam != 0 => Some(Presence::Shutdown), // zero: the shutdown was cancelled
            _ => None,
        }
    }

    unsafe extern "system" fn procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if message == WM_QUERYENDSESSION {
            return 1; // never hold up a shutdown
        }
        if message == WM_CLOSE {
            // SAFETY: our own window, on its own thread.
            unsafe { DestroyWindow(window) };
            return 0;
        }
        if message == WM_DESTROY {
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            return 0;
        }
        if let Some(event) = event_of(message, wparam) {
            let callback = report().lock().unwrap_or_else(|p| p.into_inner()).clone();
            if let Some(callback) = callback {
                callback(event);
            }
        }
        // SAFETY: the default handling of a message we were given.
        unsafe { DefWindowProcW(window, message, wparam, lparam) }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub async fn start(on_event: Callback) -> Result<Arc<AtomicIsize>> {
        *report().lock().unwrap_or_else(|p| p.into_inner()) = Some(on_event);
        let window = Arc::new(AtomicIsize::new(0));
        let (ready, started) = tokio::sync::oneshot::channel::<Result<()>>();
        let shared = window.clone();
        std::thread::Builder::new()
            .name("chsmartbulb-presence".into())
            .spawn(move || run(shared, ready))
            .map_err(|e| Error::Unsupported(e.to_string()))?;
        started.await.map_err(|_| Error::Unsupported("the session watcher did not start".into()))??;
        Ok(window)
    }

    fn run(shared: Arc<AtomicIsize>, ready: tokio::sync::oneshot::Sender<Result<()>>) {
        let name = wide("chsmartbulb.presence");
        // SAFETY: plain Win32 calls with arguments that outlive them; the window lives on this thread.
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let mut class: WNDCLASSW = std::mem::zeroed();
            class.lpfnWndProc = Some(procedure);
            class.hInstance = instance;
            class.lpszClassName = name.as_ptr();
            RegisterClassW(&class); // registering twice only fails; the old class serves
                                    // a top-level window that is never shown: message-only windows are not told about power or shutdown
            let window = CreateWindowExW(
                0,
                name.as_ptr(),
                name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            if window.is_null() {
                let _ = ready.send(Err(Error::Unsupported("cannot create the window for session events".into())));
                return;
            }
            WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION);
            shared.store(window as isize, Ordering::SeqCst);
            let _ = ready.send(Ok(()));
            let mut message: MSG = std::mem::zeroed();
            while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            WTSUnRegisterSessionNotification(window);
        }
    }

    pub fn close(window: &AtomicIsize) {
        let handle = window.swap(0, Ordering::SeqCst);
        if handle != 0 {
            // SAFETY: asks the watcher's own thread to close its window.
            unsafe { PostMessageW(handle as HWND, WM_CLOSE, 0, 0) };
        }
        *report().lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}
