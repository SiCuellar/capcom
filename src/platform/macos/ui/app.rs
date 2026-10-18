//! `capcom daemon` on macOS: NSApplication (accessory policy: menu-bar only, no Dock icon) on
//! the main thread, the tokio daemon on a background thread.
//!
//! Lifecycle: the daemon thread owns the socket. Quit (menu), SIGTERM and SIGINT all end
//! `daemon::run_with`, which removes the socket; the thread then exits the process from the main
//! queue. `applicationWillTerminate:` (logout / `NSApp.terminate`) removes the socket as a backstop.

use super::status_item::{self, StatusItem};
use super::{on_main, on_main_after, UiHost};
use crate::config::Config;
use crate::ipc::{self, Request, Response};
use anyhow::{anyhow, Result};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate};
use objc2_foundation::{MainThreadMarker, NSNotification, NSObject, NSObjectProtocol};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::Notify;

static SHUTDOWN: OnceLock<Arc<Notify>> = OnceLock::new();

/// True when this binary runs from inside a `.app` bundle (double-click, `open`, login item).
pub fn launched_from_app_bundle() -> bool {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
        .unwrap_or(false)
}

/// Blocking one-shot IPC call (for the main thread's helpers; never call it *on* the main thread
/// for anything but the startup single-instance probe).
pub fn ipc_blocking(path: &Path, req: &Request, timeout: Duration) -> Result<Response> {
    let mut s = UnixStream::connect(path)?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    s.write_all(line.as_bytes())?;
    let mut resp = String::new();
    if BufReader::new(s).read_line(&mut resp)? == 0 {
        return Err(anyhow!("daemon closed the connection"));
    }
    Ok(serde_json::from_str(&resp)?)
}

/// Does a live daemon answer on `path`? (A stale socket file does not count.)
pub fn daemon_alive(path: &Path) -> bool {
    ipc_blocking(path, &Request::Status, Duration::from_millis(1500)).is_ok()
}

/// Ask the daemon to stop (it removes the socket, then the process exits). Falls back to a hard
/// exit if the daemon has not finished within 3 s.
pub fn request_quit() {
    match SHUTDOWN.get() {
        Some(n) => {
            n.notify_one();
            on_main_after(Duration::from_secs(3), || {
                tracing::warn!("daemon did not stop in time; exiting");
                let _ = std::fs::remove_file(ipc::socket_path());
                std::process::exit(0);
            });
        }
        None => std::process::exit(0),
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; AppDelegate does not implement Drop.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "CapcomAppDelegate"]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        /// Double-clicking Capcom.app while it already runs: drop the menu down so the user sees it.
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _app: &NSApplication, _visible: bool) -> bool {
            status_item::pop_menu(self.mtm());
            false
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _n: &NSNotification) {
            let _ = std::fs::remove_file(ipc::socket_path());
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's `init` signature.
        unsafe { msg_send![super(this), init] }
    }
}

/// Run the daemon with the menu-bar UI. Never returns. Must be called on the main thread, outside
/// any tokio runtime. `bundled`: launched as Capcom.app (affects single-instance messaging).
pub fn run_daemon_app(cfg: Config, bundled: bool) -> ! {
    let mtm = MainThreadMarker::new().expect("run_daemon_app must be called on the main thread");
    let sock = ipc::socket_path();
    if daemon_alive(&sock) {
        if bundled {
            tracing::info!("Capcom is already running on {}; exiting", sock.display());
            crate::platform::macos::notify("Capcom is already running (see the mic icon in the menu bar)");
            std::process::exit(0);
        }
        eprintln!("error: another daemon is already listening on {}", sock.display());
        std::process::exit(1);
    }

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let delegate = AppDelegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    let host = UiHost::new(mtm);
    host.register(Box::new(StatusItem::new(mtm, &cfg, bundled)));
    host.register(Box::new(super::pill::Pill::new(mtm, cfg.ui.pill_position)));

    let shutdown = SHUTDOWN.get_or_init(|| Arc::new(Notify::new())).clone();
    let sink = host.sink();
    let spawned = std::thread::Builder::new().name("capcom-daemon".into()).spawn(move || {
        let res = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(anyhow::Error::from)
            .and_then(|rt| rt.block_on(crate::daemon::run_with(cfg, sink, async move { shutdown.notified().await })));
        let code = match res {
            Ok(()) => 0,
            Err(e) => {
                tracing::error!("daemon stopped: {e:#}");
                crate::platform::macos::notify(&format!("Capcom stopped: {e:#}"));
                1
            }
        };
        on_main(move || std::process::exit(code));
    });
    if let Err(e) = spawned {
        eprintln!("error: cannot start the daemon thread: {e}");
        std::process::exit(1);
    }

    app.run();
    // `run` only returns after `stop:`; nothing calls it, but exit cleanly if it ever does.
    let _ = std::fs::remove_file(ipc::socket_path());
    std::process::exit(0)
}
