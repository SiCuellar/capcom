//! Sounds (afplay) and notifications (osascript).

use crate::platform::{Feedback, FeedbackEvent};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const START: &[u8] = include_bytes!("../../../assets/sounds/start.wav");
const STOP: &[u8] = include_bytes!("../../../assets/sounds/stop.wav");
const ERROR: &[u8] = include_bytes!("../../../assets/sounds/error.wav");
const CANCEL: &[u8] = include_bytes!("../../../assets/sounds/cancel.wav");

pub struct MacFeedback {
    dir: PathBuf,
}

impl MacFeedback {
    pub fn new() -> Self {
        let dir = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("Capcom").join("sounds");
        Self { dir }
    }

    /// Write the embedded sound to the cache dir if missing or stale, then play it.
    fn play(&self, name: &str, data: &'static [u8]) {
        let path = self.dir.join(name);
        let fresh = std::fs::metadata(&path).map(|m| m.len() == data.len() as u64).unwrap_or(false);
        if !fresh {
            if let Err(e) = std::fs::create_dir_all(&self.dir).and_then(|_| std::fs::write(&path, data)) {
                tracing::warn!("cannot write sound {}: {e}", path.display());
                return;
            }
        }
        spawn_reaped(Command::new("afplay").arg("-v").arg("0.6").arg(&path));
    }
}

impl Default for MacFeedback {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn a child without waiting on the caller's thread, but reap it so no zombies pile up.
fn spawn_reaped(cmd: &mut Command) {
    match cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => tracing::debug!("spawn failed: {e}"),
    }
}

/// Escape a string for embedding in an AppleScript double-quoted literal.
pub fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', " ")
}

pub fn notify(msg: &str) {
    let script = format!("display notification \"{}\" with title \"Capcom\"", applescript_escape(msg));
    spawn_reaped(Command::new("osascript").arg("-e").arg(script));
}

impl Feedback for MacFeedback {
    fn event(&self, ev: FeedbackEvent) {
        match ev {
            FeedbackEvent::RecordingStarted => self.play("start.wav", START),
            FeedbackEvent::RecordingStopped => self.play("stop.wav", STOP),
            FeedbackEvent::Busy | FeedbackEvent::Discarded => self.play("cancel.wav", CANCEL),
            FeedbackEvent::Done => {}
            FeedbackEvent::Error(m) => {
                tracing::warn!("{m}");
                self.play("error.wav", ERROR);
                notify(&m);
            }
            FeedbackEvent::Notify(m) => notify(&m),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_quotes_and_backslashes() {
        assert_eq!(applescript_escape("a \"b\" \\ c\nd"), "a \\\"b\\\" \\\\ c d");
    }
}
