//! Pure state machine from architecture.md. No I/O: `step` returns the next state plus an
//! [`Action`] telling the daemon what to do.
//!
//! ```text
//! Idle --start--> Recording --stop--> Transcribing --> Cleaning --> Pasting --> Idle
//!                   |  cancel / <min_clip           \--error------/--> paste raw + notify
//! (start while busy: ignored, "busy" feedback)
//! ```

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum State {
    #[default]
    Idle,
    Recording,
    Transcribing,
    Cleaning,
    Pasting,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Recording => "recording",
            State::Transcribing => "transcribing",
            State::Cleaning => "cleaning",
            State::Pasting => "pasting",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Start,
    /// Stop recording; `clip_ms` is the captured duration.
    Stop { clip_ms: u64 },
    Cancel,
    /// ASR finished; cleanup will run.
    AsrDone,
    /// ASR finished and cleanup is skipped/disabled: go straight to pasting.
    AsrDoneSkipCleanup,
    /// ASR failed: nothing to paste.
    AsrFailed,
    CleanupDone,
    /// Cleanup error/timeout/length: paste the raw text.
    CleanupFailed,
    PasteDone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// Open the mic, begin capture (start feedback after first samples).
    BeginCapture,
    /// Stop capture and run the ASR -> cleanup pipeline.
    RunPipeline,
    /// Throw the clip away (cancelled).
    DiscardCancelled,
    /// Throw the clip away (shorter than min clip).
    DiscardTooShort,
    /// Play the "busy" sound; the request is ignored.
    Busy,
    /// Paste the final text (cleaned).
    Paste,
    /// Paste the raw text and notify the user of the error.
    PasteRawAndNotify,
    /// Nothing to paste; notify the user of the error.
    NotifyError,
    /// Event was not valid in this state; ignored.
    Ignored,
}

pub fn step(state: State, event: Event, min_clip_ms: u64) -> (State, Action) {
    use Action as A;
    use Event as E;
    use State as S;
    match (state, event) {
        (S::Idle, E::Start) => (S::Recording, A::BeginCapture),
        (s, E::Start) => (s, if s == S::Recording { A::Ignored } else { A::Busy }),

        (S::Recording, E::Stop { clip_ms }) if clip_ms < min_clip_ms => (S::Idle, A::DiscardTooShort),
        (S::Recording, E::Stop { .. }) => (S::Transcribing, A::RunPipeline),
        (S::Recording, E::Cancel) => (S::Idle, A::DiscardCancelled),

        (S::Transcribing, E::AsrDone) => (S::Cleaning, A::None),
        (S::Transcribing, E::AsrDoneSkipCleanup) => (S::Pasting, A::Paste),
        (S::Transcribing, E::AsrFailed) => (S::Idle, A::NotifyError),

        (S::Cleaning, E::CleanupDone) => (S::Pasting, A::Paste),
        (S::Cleaning, E::CleanupFailed) => (S::Pasting, A::PasteRawAndNotify),

        (S::Pasting, E::PasteDone) => (S::Idle, A::None),

        (s, _) => (s, A::Ignored),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Action as A;
    use Event as E;
    use State as S;
    const MIN: u64 = 500;

    #[test]
    fn happy_path() {
        let (s, a) = step(S::Idle, E::Start, MIN);
        assert_eq!((s, a), (S::Recording, A::BeginCapture));
        let (s, a) = step(s, E::Stop { clip_ms: 2000 }, MIN);
        assert_eq!((s, a), (S::Transcribing, A::RunPipeline));
        let (s, _) = step(s, E::AsrDone, MIN);
        assert_eq!(s, S::Cleaning);
        let (s, a) = step(s, E::CleanupDone, MIN);
        assert_eq!((s, a), (S::Pasting, A::Paste));
        assert_eq!(step(s, E::PasteDone, MIN), (S::Idle, A::None));
    }

    #[test]
    fn min_clip_rule() {
        assert_eq!(step(S::Recording, E::Stop { clip_ms: 499 }, MIN), (S::Idle, A::DiscardTooShort));
        assert_eq!(step(S::Recording, E::Stop { clip_ms: 500 }, MIN), (S::Transcribing, A::RunPipeline));
    }

    #[test]
    fn busy_rule() {
        for s in [S::Transcribing, S::Cleaning, S::Pasting] {
            assert_eq!(step(s, E::Start, MIN), (s, A::Busy));
        }
        assert_eq!(step(S::Recording, E::Start, MIN), (S::Recording, A::Ignored));
    }

    #[test]
    fn cancel_only_while_recording() {
        assert_eq!(step(S::Recording, E::Cancel, MIN), (S::Idle, A::DiscardCancelled));
        assert_eq!(step(S::Cleaning, E::Cancel, MIN), (S::Cleaning, A::Ignored));
        assert_eq!(step(S::Idle, E::Cancel, MIN), (S::Idle, A::Ignored));
    }

    #[test]
    fn error_paths() {
        assert_eq!(step(S::Cleaning, E::CleanupFailed, MIN), (S::Pasting, A::PasteRawAndNotify));
        assert_eq!(step(S::Transcribing, E::AsrFailed, MIN), (S::Idle, A::NotifyError));
        assert_eq!(step(S::Transcribing, E::AsrDoneSkipCleanup, MIN), (S::Pasting, A::Paste));
    }

    #[test]
    fn stop_when_idle_ignored() {
        assert_eq!(step(S::Idle, E::Stop { clip_ms: 9999 }, MIN), (S::Idle, A::Ignored));
    }
}
