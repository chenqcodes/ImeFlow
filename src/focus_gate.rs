//! Pure focus-epoch gate: confirm a target, handle once, then respect manual changes.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Confirm,
    Ready,
    AlreadyHandled,
}

pub struct FocusGate<K, M> {
    candidate: Option<(K, M, Instant)>,
    handled: Option<K>,
    quiet: Duration,
}

impl<K: Clone + Eq, M: Copy + Eq> FocusGate<K, M> {
    pub fn new(quiet: Duration) -> Self {
        Self {
            candidate: None,
            handled: None,
            quiet,
        }
    }

    pub fn reset(&mut self) {
        self.candidate = None;
        self.handled = None;
    }

    pub fn observe(&mut self, key: K, mode: M, now: Instant) -> Decision {
        // Classification changes alone must not override the same element again.
        if self.handled.as_ref() == Some(&key) {
            return Decision::AlreadyHandled;
        }
        self.handled = None;
        match &self.candidate {
            Some((old, old_mode, since)) if old == &key && *old_mode == mode => {
                if now.saturating_duration_since(*since) >= self.quiet {
                    Decision::Ready
                } else {
                    Decision::Confirm
                }
            }
            _ => {
                self.candidate = Some((key, mode, now));
                Decision::Confirm
            }
        }
    }

    pub fn complete(&mut self, key: K) {
        self.handled = Some(key);
        self.candidate = None;
    }

    /// Re-arm only when execution proves no key/write occurred. Start a new quiet interval.
    pub fn retry_without_effect(&mut self, key: &K) {
        if self.handled.as_ref() == Some(key) {
            self.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_write_failure_must_settle_again_but_cannot_reset_another_scope() {
        let t = Instant::now();
        let mut gate = FocusGate::new(Duration::from_millis(100));
        gate.complete("terminal");
        gate.retry_without_effect(&"other");
        assert_eq!(gate.observe("terminal", "en", t), Decision::AlreadyHandled);
        gate.retry_without_effect(&"terminal");
        assert_eq!(gate.observe("terminal", "en", t), Decision::Confirm);
        assert_eq!(
            gate.observe("terminal", "en", t + Duration::from_millis(120)),
            Decision::Ready
        );
    }

    #[test]
    fn focus_storm_requires_stable_confirmation() {
        let t = Instant::now();
        let mut gate = FocusGate::new(Duration::from_millis(100));
        assert_eq!(gate.observe("terminal", "en", t), Decision::Confirm);
        assert_eq!(
            gate.observe("terminal", "en", t + Duration::from_millis(60)),
            Decision::Confirm
        );
        assert_eq!(
            gate.observe("terminal", "en", t + Duration::from_millis(120)),
            Decision::Ready
        );
        gate.complete("terminal");
        assert_eq!(
            gate.observe("terminal", "en", t + Duration::from_secs(2)),
            Decision::AlreadyHandled
        );
    }

    #[test]
    fn same_element_misclassification_does_not_switch_back() {
        let t = Instant::now();
        let mut gate = FocusGate::new(Duration::ZERO);
        gate.complete("runtime-id-1");
        assert_eq!(
            gate.observe("runtime-id-1", "zh", t),
            Decision::AlreadyHandled
        );
    }

    #[test]
    fn actual_focus_change_and_return_rearms() {
        let t = Instant::now();
        let mut gate = FocusGate::new(Duration::ZERO);
        gate.complete("terminal");
        assert_eq!(gate.observe("chat", "zh", t), Decision::Confirm);
        assert_eq!(gate.observe("chat", "zh", t), Decision::Ready);
        gate.complete("chat");
        assert_eq!(gate.observe("terminal", "en", t), Decision::Confirm);
        assert_eq!(gate.observe("terminal", "en", t), Decision::Ready);
    }

    #[test]
    fn changed_rule_must_settle_before_first_action() {
        let t = Instant::now();
        let mut gate = FocusGate::new(Duration::from_millis(100));
        gate.observe("input", "en", t);
        assert_eq!(
            gate.observe("input", "zh", t + Duration::from_millis(120)),
            Decision::Confirm
        );
        assert_eq!(
            gate.observe("input", "zh", t + Duration::from_millis(150)),
            Decision::Confirm
        );
        assert_eq!(
            gate.observe("input", "zh", t + Duration::from_millis(240)),
            Decision::Ready
        );
    }

    #[test]
    fn leaving_window_rearms_even_when_returning_to_same_element() {
        let mut gate = FocusGate::new(Duration::ZERO);
        gate.complete("terminal");
        gate.reset();
        assert_eq!(
            gate.observe("terminal", "en", Instant::now()),
            Decision::Confirm
        );
    }
}
