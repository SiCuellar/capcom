//! Pure hotkey logic (no OS calls): translating raw key events into a trigger/cancel/other
//! classification, and the tap / hold / double-tap / cancel state machine. Time is passed in
//! explicitly so everything is unit-testable.

use crate::platform::HotkeyEvent;
use handy_keys::{Hotkey, Key, KeyEvent, Modifiers};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// A release sooner than this after the press is a "tap" rather than a hold.
    pub tap_max: Duration,
    /// Max gap between the first tap's release and the second press for a double-tap.
    pub double_tap: Duration,
    /// Another key pressed within this long after the trigger went down means "not dictation".
    pub cancel_window: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            tap_max: Duration::from_millis(250),
            double_tap: Duration::from_millis(400),
            cancel_window: Duration::from_millis(300),
        }
    }
}

/// What a raw key event means for us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    TriggerDown,
    TriggerUp,
    /// Some unrelated key went down.
    OtherKey,
    /// The cancel key (Esc) went down.
    CancelKey,
}

/// Turns raw `KeyEvent`s into `Input`s, tracking whether the trigger is currently held.
pub struct Classifier {
    trigger: Hotkey,
    cancel: Hotkey,
    trigger_down: bool,
}

const GROUPS: [Modifiers; 5] = [Modifiers::CMD, Modifiers::SHIFT, Modifiers::CTRL, Modifiers::OPT, Modifiers::FN];

fn is_mouse(k: Key) -> bool {
    matches!(k, Key::MouseLeft | Key::MouseRight | Key::MouseMiddle | Key::MouseX1 | Key::MouseX2)
}

impl Classifier {
    pub fn new(trigger: Hotkey, cancel: Hotkey) -> Self {
        Self { trigger, cancel, trigger_down: false }
    }

    /// Is a modifier-only trigger satisfied by `mods`? Modifier groups the trigger does not
    /// mention are ignored, so holding Shift while holding Fn does not end the dictation.
    fn modifier_trigger_active(&self, mods: Modifiers) -> bool {
        let mut masked = mods;
        for g in GROUPS {
            if !self.trigger.modifiers.intersects(g) {
                masked.remove(g);
            }
        }
        self.trigger.modifiers.matches(masked)
    }

    pub fn classify(&mut self, ev: &KeyEvent) -> Option<Input> {
        match (self.trigger.key, ev.key) {
            // Modifier-only trigger (e.g. Fn): only flagsChanged events carry the state.
            (None, None) if ev.changed_modifier.is_some() => {
                let active = self.modifier_trigger_active(ev.modifiers);
                if active == self.trigger_down {
                    return None;
                }
                self.trigger_down = active;
                Some(if active { Input::TriggerDown } else { Input::TriggerUp })
            }
            (None, None) => None,
            // Key trigger (e.g. F13, or Option+Space).
            (Some(tk), Some(k)) if tk == k => {
                if ev.is_key_down {
                    if self.trigger_down || !self.trigger.modifiers.matches(ev.modifiers) {
                        return None; // autorepeat or wrong modifiers
                    }
                    self.trigger_down = true;
                    Some(Input::TriggerDown)
                } else if self.trigger_down {
                    self.trigger_down = false;
                    Some(Input::TriggerUp)
                } else {
                    None
                }
            }
            (_, Some(k)) if ev.is_key_down && !is_mouse(k) => {
                if self.cancel.key == Some(k) && self.cancel.modifiers.matches(ev.modifiers) {
                    Some(Input::CancelKey)
                } else {
                    Some(Input::OtherKey)
                }
            }
            // A modifier-trigger released while it is a *key* trigger's modifier, etc.: ignore,
            // except a modifier-only trigger releasing through a key-bearing event is impossible.
            _ => {
                // Key trigger whose modifier was released mid-hold: treat as release.
                if self.trigger.key.is_some() && self.trigger_down && ev.key.is_none() && ev.changed_modifier.is_some() && !self.trigger.modifiers.matches(ev.modifiers) {
                    self.trigger_down = false;
                    return Some(Input::TriggerUp);
                }
                None
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    /// Trigger held, recording (push-to-talk).
    Holding { down_at: Instant },
    /// A short tap happened; recording continues while we wait for a possible second tap.
    TapPending { released_at: Instant },
    /// Hands-free: recording until the next trigger press.
    Latched,
    /// Ignore everything until the trigger is released (after cancel / latched stop).
    WaitRelease,
}

pub struct Machine {
    pub phase: Phase,
    timing: Timing,
}

impl Machine {
    pub fn new(timing: Timing) -> Self {
        Self { phase: Phase::Idle, timing }
    }

    /// True while a dictation is in progress (so the cancel key should be consumed).
    pub fn is_active(&self) -> bool {
        matches!(self.phase, Phase::Holding { .. } | Phase::TapPending { .. } | Phase::Latched)
    }

    /// When `tick` should next be called, if anything is time-driven.
    pub fn deadline(&self) -> Option<Instant> {
        match self.phase {
            Phase::TapPending { released_at } => Some(released_at + self.timing.double_tap),
            _ => None,
        }
    }

    /// Time-driven transitions: a lone tap whose double-tap window expired is cancelled.
    pub fn tick(&mut self, now: Instant) -> Option<HotkeyEvent> {
        if let Phase::TapPending { released_at } = self.phase {
            if now.duration_since(released_at) >= self.timing.double_tap {
                self.phase = Phase::Idle;
                return Some(HotkeyEvent::Cancel);
            }
        }
        None
    }

    pub fn step(&mut self, now: Instant, input: Input) -> Option<HotkeyEvent> {
        use HotkeyEvent as E;
        use Phase::*;
        // An expired tap window must resolve before interpreting the next input.
        let expired = self.tick(now);
        let out = match (self.phase, input) {
            (Idle, Input::TriggerDown) => {
                self.phase = Holding { down_at: now };
                Some(E::Start)
            }
            (Holding { down_at }, Input::TriggerUp) => {
                if now.duration_since(down_at) < self.timing.tap_max {
                    self.phase = TapPending { released_at: now };
                    None
                } else {
                    self.phase = Idle;
                    Some(E::Stop)
                }
            }
            (Holding { down_at }, Input::OtherKey) => {
                if now.duration_since(down_at) <= self.timing.cancel_window {
                    self.phase = WaitRelease;
                    Some(E::Cancel)
                } else {
                    None
                }
            }
            (TapPending { .. }, Input::TriggerDown) => {
                self.phase = Latched;
                None
            }
            (Latched, Input::TriggerDown) => {
                self.phase = WaitRelease;
                Some(E::Stop)
            }
            (Holding { .. }, Input::CancelKey) => {
                self.phase = WaitRelease;
                Some(E::Cancel)
            }
            (TapPending { .. } | Latched, Input::CancelKey) => {
                self.phase = Idle;
                Some(E::Cancel)
            }
            (WaitRelease, Input::TriggerUp) => {
                self.phase = Idle;
                None
            }
            _ => None,
        };
        // `expired` can only be Some when we were in TapPending, in which case `out` is
        // computed against Idle and is at most a fresh Start; report the cancel first.
        match (expired, out) {
            (Some(c), None) => Some(c),
            (Some(_), Some(o)) => Some(o), // cancel + new start: daemon treats Start after discard fine
            (None, o) => o,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use HotkeyEvent as E;

    fn ms(base: Instant, n: u64) -> Instant {
        base + Duration::from_millis(n)
    }

    fn m() -> (Machine, Instant) {
        (Machine::new(Timing::default()), Instant::now())
    }

    #[test]
    fn hold_is_push_to_talk() {
        let (mut m, t) = m();
        assert_eq!(m.step(t, Input::TriggerDown), Some(E::Start));
        assert!(m.is_active());
        assert_eq!(m.step(ms(t, 1500), Input::TriggerUp), Some(E::Stop));
        assert!(!m.is_active());
    }

    #[test]
    fn double_tap_latches_then_tap_stops() {
        let (mut m, t) = m();
        assert_eq!(m.step(t, Input::TriggerDown), Some(E::Start));
        assert_eq!(m.step(ms(t, 100), Input::TriggerUp), None);
        assert!(m.is_active());
        assert_eq!(m.step(ms(t, 300), Input::TriggerDown), None); // latched
        assert_eq!(m.step(ms(t, 380), Input::TriggerUp), None);
        assert_eq!(m.phase, Phase::Latched);
        // much later: tap to stop
        assert_eq!(m.step(ms(t, 9000), Input::TriggerDown), Some(E::Stop));
        assert_eq!(m.step(ms(t, 9080), Input::TriggerUp), None);
        assert_eq!(m.phase, Phase::Idle);
    }

    #[test]
    fn lone_tap_cancels_after_window() {
        let (mut m, t) = m();
        m.step(t, Input::TriggerDown);
        m.step(ms(t, 100), Input::TriggerUp);
        assert_eq!(m.deadline(), Some(ms(t, 500)));
        assert_eq!(m.tick(ms(t, 400)), None);
        assert_eq!(m.tick(ms(t, 500)), Some(E::Cancel));
        assert_eq!(m.phase, Phase::Idle);
    }

    #[test]
    fn second_tap_too_late_is_a_fresh_start() {
        let (mut m, t) = m();
        m.step(t, Input::TriggerDown);
        m.step(ms(t, 100), Input::TriggerUp);
        // window expired before the tick fired; new press must still start fresh
        assert_eq!(m.step(ms(t, 700), Input::TriggerDown), Some(E::Start));
        assert!(matches!(m.phase, Phase::Holding { .. }));
    }

    #[test]
    fn other_key_soon_after_press_cancels_and_release_is_ignored() {
        let (mut m, t) = m();
        m.step(t, Input::TriggerDown);
        assert_eq!(m.step(ms(t, 120), Input::OtherKey), Some(E::Cancel));
        assert!(!m.is_active());
        assert_eq!(m.step(ms(t, 900), Input::TriggerUp), None);
        assert_eq!(m.phase, Phase::Idle);
    }

    #[test]
    fn other_key_late_in_hold_is_ignored() {
        let (mut m, t) = m();
        m.step(t, Input::TriggerDown);
        assert_eq!(m.step(ms(t, 800), Input::OtherKey), None);
        assert_eq!(m.step(ms(t, 1200), Input::TriggerUp), Some(E::Stop));
    }

    #[test]
    fn esc_cancels_in_every_active_phase() {
        let (mut m, t) = m();
        m.step(t, Input::TriggerDown);
        assert_eq!(m.step(ms(t, 1000), Input::CancelKey), Some(E::Cancel));
        assert_eq!(m.step(ms(t, 1500), Input::TriggerUp), None); // no Stop after cancel
        assert_eq!(m.phase, Phase::Idle);

        // latched
        let t = ms(t, 5000);
        m.step(t, Input::TriggerDown);
        m.step(ms(t, 50), Input::TriggerUp);
        m.step(ms(t, 200), Input::TriggerDown);
        m.step(ms(t, 250), Input::TriggerUp);
        assert_eq!(m.step(ms(t, 3000), Input::CancelKey), Some(E::Cancel));
        assert_eq!(m.phase, Phase::Idle);

        // idle: nothing, and not active (so Esc is not consumed)
        assert_eq!(m.step(ms(t, 4000), Input::CancelKey), None);
        assert!(!m.is_active());
    }

    // --- classifier ---

    fn kev(mods: Modifiers, key: Option<Key>, down: bool, changed: Option<Modifiers>) -> KeyEvent {
        KeyEvent { modifiers: mods, key, is_key_down: down, changed_modifier: changed }
    }

    fn fn_cls() -> Classifier {
        Classifier::new("fn".parse().unwrap(), "escape".parse().unwrap())
    }

    #[test]
    fn classifier_fn_press_release() {
        let mut c = fn_cls();
        assert_eq!(c.classify(&kev(Modifiers::FN, None, true, Some(Modifiers::FN))), Some(Input::TriggerDown));
        // Shift added while Fn held: still down, no event
        assert_eq!(c.classify(&kev(Modifiers::FN | Modifiers::SHIFT_LEFT, None, true, Some(Modifiers::SHIFT_LEFT))), None);
        assert_eq!(c.classify(&kev(Modifiers::SHIFT_LEFT, None, false, Some(Modifiers::FN))), Some(Input::TriggerUp));
    }

    #[test]
    fn classifier_right_ctrl_only() {
        let mut c = Classifier::new("ctrlright".parse().unwrap(), "escape".parse().unwrap());
        // Left Ctrl (normal shortcuts) must not trigger.
        assert_eq!(c.classify(&kev(Modifiers::CTRL_LEFT, None, true, Some(Modifiers::CTRL_LEFT))), None);
        assert_eq!(c.classify(&kev(Modifiers::empty(), None, false, Some(Modifiers::CTRL_LEFT))), None);
        // Right Ctrl press/release.
        assert_eq!(c.classify(&kev(Modifiers::CTRL_RIGHT, None, true, Some(Modifiers::CTRL_RIGHT))), Some(Input::TriggerDown));
        assert_eq!(c.classify(&kev(Modifiers::empty(), None, false, Some(Modifiers::CTRL_RIGHT))), Some(Input::TriggerUp));
    }

    #[test]
    fn classifier_keys() {
        let mut c = fn_cls();
        assert_eq!(c.classify(&kev(Modifiers::empty(), Some(Key::Escape), true, None)), Some(Input::CancelKey));
        assert_eq!(c.classify(&kev(Modifiers::empty(), Some(Key::A), true, None)), Some(Input::OtherKey));
        assert_eq!(c.classify(&kev(Modifiers::empty(), Some(Key::A), false, None)), None);
        assert_eq!(c.classify(&kev(Modifiers::FN, Some(Key::MouseLeft), true, None)), None);
    }

    #[test]
    fn classifier_key_trigger_ignores_autorepeat() {
        let mut c = Classifier::new("f13".parse().unwrap(), "escape".parse().unwrap());
        assert_eq!(c.classify(&kev(Modifiers::empty(), Some(Key::F13), true, None)), Some(Input::TriggerDown));
        assert_eq!(c.classify(&kev(Modifiers::empty(), Some(Key::F13), true, None)), None);
        assert_eq!(c.classify(&kev(Modifiers::empty(), Some(Key::F13), false, None)), Some(Input::TriggerUp));
    }
}
