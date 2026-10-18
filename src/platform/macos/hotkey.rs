//! Global hotkey via `handy-keys` (CGEventTap). See `hotkey_logic` for the state machine.

use super::hotkey_logic::{Classifier, Machine, Timing};
use crate::config::HotkeyConfig;
use crate::platform::{Hotkey, HotkeyEvent};
use anyhow::{anyhow, Result};
use handy_keys::{Error as HkError, Hotkey as HkHotkey, KeyboardListener};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

pub struct MacHotkey {
    trigger: HkHotkey,
    cancel: HkHotkey,
}

impl MacHotkey {
    pub fn new(cfg: &HotkeyConfig) -> Self {
        let parse = |s: &str, default: &str, what: &str| -> HkHotkey {
            s.parse().unwrap_or_else(|e| {
                tracing::warn!("invalid [hotkey] {what} {s:?} ({e}); using {default:?}");
                default.parse().expect("default hotkey parses")
            })
        };
        Self { trigger: parse(&cfg.push_to_talk, "ctrlright", "push_to_talk"), cancel: parse(&cfg.cancel, "escape", "cancel") }
    }
}

impl Hotkey for MacHotkey {
    fn run(&mut self, tx: UnboundedSender<HotkeyEvent>) -> Result<()> {
        if !handy_keys::check_accessibility() {
            return Err(anyhow!(
                "Accessibility permission not granted: the hotkey is disabled (CLI start/stop still works). \
                 Grant it in System Settings > Privacy & Security > Accessibility (and Input Monitoring) \
                 to the Capcom app; run `capcom doctor` for the links"
            ));
        }
        // Blocking set: contains the cancel key only while a dictation is active, so Esc is
        // consumed during recording and passes through normally otherwise.
        let blocked: Arc<Mutex<HashSet<HkHotkey>>> = Arc::new(Mutex::new(HashSet::new()));
        let listener = KeyboardListener::new_with_blocking(blocked.clone())
            .map_err(|e| anyhow!("cannot create the keyboard event tap: {e} (needs Accessibility + Input Monitoring)"))?;
        tracing::info!("hotkey active: trigger={} cancel={}", self.trigger, self.cancel);

        let mut classifier = Classifier::new(self.trigger, self.cancel);
        let mut machine = Machine::new(Timing::default());
        let mut cancel_blocked = false;

        loop {
            let timeout = machine
                .deadline()
                .map(|d| d.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::from_secs(3600));
            let out = match listener.recv_timeout(timeout) {
                Ok(ev) => classifier.classify(&ev).and_then(|i| machine.step(Instant::now(), i)),
                Err(HkError::Timeout) => machine.tick(Instant::now()),
                Err(e) => return Err(anyhow!("keyboard listener stopped: {e}")),
            };
            let active = machine.is_active();
            if active != cancel_blocked {
                cancel_blocked = active;
                if let Ok(mut set) = blocked.lock() {
                    if active {
                        set.insert(self.cancel);
                    } else {
                        set.remove(&self.cancel);
                    }
                }
            }
            if let Some(e) = out {
                tracing::debug!("hotkey event {e:?}");
                if tx.send(e).is_err() {
                    return Ok(()); // daemon gone
                }
            }
        }
    }
}
