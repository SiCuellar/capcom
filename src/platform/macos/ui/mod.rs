//! macOS app host: AppKit on the main thread, the daemon on a background thread.
//!
//! - [`UiHost`] owns the registered [`UiComponent`]s (status item, pill, ...). Register any
//!   number of them before [`app::run_daemon_app`] enters the run loop.
//! - [`MainThreadSink`] is the [`UiSink`] the daemon gets: `send` is non-blocking from any thread
//!   and hops to the main dispatch queue (drained by the NSApplication run loop, also while a menu
//!   is open), where the event is fanned out to every component in registration order.

pub mod app;
pub mod pill;
pub mod status_item;

use crate::ui::{UiComponent, UiEvent, UiSink};
use dispatch2::{DispatchQueue, DispatchTime};
use objc2_foundation::MainThreadMarker;
use std::cell::RefCell;
use std::sync::Arc;
use std::time::Duration;

pub use app::run_daemon_app;

thread_local! {
    /// Lives on the main thread only (registration requires a `MainThreadMarker`, and the fan-out
    /// runs on the main queue).
    static COMPONENTS: RefCell<Vec<Box<dyn UiComponent>>> = const { RefCell::new(Vec::new()) };
}

/// Main-thread registry of UI components.
pub struct UiHost {
    _mtm: MainThreadMarker,
}

impl UiHost {
    pub fn new(mtm: MainThreadMarker) -> Self {
        Self { _mtm: mtm }
    }

    /// Add a component; it receives every [`UiEvent`] from now on, on the main thread.
    pub fn register(&self, component: Box<dyn UiComponent>) {
        COMPONENTS.with(|c| c.borrow_mut().push(component));
    }

    /// The sink to hand to the daemon.
    pub fn sink(&self) -> Arc<dyn UiSink> {
        Arc::new(MainThreadSink)
    }
}

/// Fans out on the main thread. Cheap and non-blocking from any thread.
pub struct MainThreadSink;

impl UiSink for MainThreadSink {
    fn send(&self, ev: UiEvent) {
        DispatchQueue::main().exec_async(move || deliver(&ev));
    }
}

/// Must run on the main thread.
fn deliver(ev: &UiEvent) {
    COMPONENTS.with(|c| match c.try_borrow_mut() {
        Ok(mut cs) => cs.iter_mut().for_each(|c| c.handle(ev)),
        // Only possible if a component spun a nested run loop inside `handle` (it must not).
        Err(_) => tracing::debug!("ui event dropped (re-entrant delivery): {ev:?}"),
    });
}

/// Run `f` on the main thread (asynchronously).
pub fn on_main(f: impl FnOnce() + Send + 'static) {
    DispatchQueue::main().exec_async(f);
}

/// Run `f` on the main thread after `delay`.
pub fn on_main_after(delay: Duration, f: impl FnOnce() + Send + 'static) {
    match DispatchTime::try_from(delay) {
        Ok(t) => {
            let _ = DispatchQueue::main().after(t, f);
        }
        Err(()) => on_main(f),
    }
}
