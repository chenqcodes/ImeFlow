//! Pure switching policy. Execution and reporting do not decide whether another toggle is safe.
use super::{ActiveIme, ImeSnapshot, LayerObservation, TerminalEnPlan};
use crate::{mode::TargetMode, uia::ControlKind};
use std::time::{Duration, Instant};

/// Focus quiet does not imply mode quiet: WeChat can restore then synchronize a context.
#[derive(Default)]
pub struct WeChatSettling {
    previous: Option<ModeSample>,
}
struct ModeSample {
    key: String,
    generation: u64,
    open: Option<bool>,
    native: Option<bool>,
    since: Instant,
}

impl WeChatSettling {
    pub fn ready(
        &mut self,
        key: &str,
        generation: u64,
        snapshot: &ImeSnapshot,
        now: Instant,
    ) -> bool {
        if snapshot.layout.backend != ActiveIme::WeChatIme {
            self.previous = None;
            return true;
        }
        if let Some(previous) = &self.previous {
            if previous.key == key
                && previous.generation == generation
                && previous.open == snapshot.open
                && previous.native == snapshot.conversion_native
            {
                return now.saturating_duration_since(previous.since) >= Duration::from_millis(240);
            }
        }
        self.previous = Some(ModeSample {
            key: key.into(),
            generation,
            open: snapshot.open,
            native: snapshot.conversion_native,
            since: now,
        });
        false
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SwitchPlan {
    Apply,
    Skip { mode: TargetMode, confirmed: bool },
    Defer,
}

/// Execution-time guard, after the initial planner and any terminal focus wait.
pub fn wechat_recheck(
    first: &ImeSnapshot,
    current: &ImeSnapshot,
    target: TargetMode,
) -> SwitchPlan {
    if first.layout.backend != ActiveIme::WeChatIme
        || current.layout.backend != ActiveIme::WeChatIme
        || first.open != current.open
        || first.conversion_native != current.conversion_native
        || !matches!(target, TargetMode::NativeChinese | TargetMode::ImeEnglish)
    {
        return SwitchPlan::Defer;
    }
    plan(current, target, ControlKind::Edit, None)
}

pub fn plan(
    snapshot: &ImeSnapshot,
    target: TargetMode,
    kind: ControlKind,
    memory: Option<TargetMode>,
) -> SwitchPlan {
    let observation = snapshot.observe_layer();
    if snapshot.layout.backend == ActiveIme::WeChatIme
        && matches!(target, TargetMode::ImeEnglish | TargetMode::NativeChinese)
    {
        // A prior requested mode cannot track manual changes in another app or restored
        // input contexts. Only the settled live sample participates; it is never confirmed.
        return match observation {
            LayerObservation::Probable(mode) if mode == target => SwitchPlan::Skip {
                mode,
                confirmed: false,
            },
            LayerObservation::Probable(mode) if mode.opposite_ime_layer(target) => {
                SwitchPlan::Apply
            }
            _ => SwitchPlan::Defer,
        };
    }
    if kind == ControlKind::Terminal && target == TargetMode::ImeEnglish {
        return match super::plan_terminal_english(snapshot.layout.backend, observation, memory) {
            TerminalEnPlan::Shift => SwitchPlan::Apply,
            TerminalEnPlan::Defer => SwitchPlan::Defer,
            TerminalEnPlan::Skip => SwitchPlan::Skip {
                mode: if snapshot.layout.backend == ActiveIme::LayoutEnglish {
                    TargetMode::LayoutEnglish
                } else {
                    target
                },
                confirmed: true,
            },
        };
    }
    let matches = match target {
        TargetMode::LayoutEnglish => snapshot.layout.backend == ActiveIme::LayoutEnglish,
        TargetMode::NativeChinese | TargetMode::ImeEnglish => {
            snapshot.resolve_layer(memory) == Some(target)
        }
        TargetMode::Passthrough => false,
    };
    if matches {
        SwitchPlan::Skip {
            mode: target,
            confirmed: matches!(observation, LayerObservation::Confident(mode) if mode == target),
        }
    } else {
        SwitchPlan::Apply
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(backend: ActiveIme, native: Option<bool>) -> ImeSnapshot {
        ImeSnapshot {
            layout: super::super::LayoutInfo {
                lang_id: 0x0804,
                backend,
            },
            conversion_native: native,
            open: Some(true),
        }
    }
    #[test]
    fn terminal_unknown_never_toggles_from_memory() {
        assert_eq!(
            plan(
                &snapshot(ActiveIme::MsPinyin, None),
                TargetMode::ImeEnglish,
                ControlKind::Terminal,
                Some(TargetMode::NativeChinese)
            ),
            SwitchPlan::Defer
        );
    }
    #[test]
    fn wechat_english_is_not_toggled_again_in_any_region() {
        for kind in [
            ControlKind::Terminal,
            ControlKind::AddressBar,
            ControlKind::Other,
        ] {
            assert_eq!(
                plan(
                    &snapshot(ActiveIme::WeChatIme, Some(false)),
                    TargetMode::ImeEnglish,
                    kind,
                    Some(TargetMode::ImeEnglish)
                ),
                SwitchPlan::Skip {
                    mode: TargetMode::ImeEnglish,
                    confirmed: false
                }
            );
        }
    }
    #[test]
    fn soft_skip_is_never_confirmed() {
        assert_eq!(
            plan(
                &snapshot(ActiveIme::WeChatIme, Some(true)),
                TargetMode::NativeChinese,
                ControlKind::Edit,
                Some(TargetMode::NativeChinese)
            ),
            SwitchPlan::Skip {
                mode: TargetMode::NativeChinese,
                confirmed: false
            }
        );
    }
    #[test]
    fn actual_eng_layout_is_reported_as_layout_not_ime_layer() {
        assert_eq!(
            plan(
                &snapshot(ActiveIme::LayoutEnglish, None),
                TargetMode::ImeEnglish,
                ControlKind::Terminal,
                None
            ),
            SwitchPlan::Skip {
                mode: TargetMode::LayoutEnglish,
                confirmed: true
            }
        );
    }

    #[test]
    fn wechat_closed_native_bit_replays_terminal_browser_round_trips_without_toggle() {
        let mut s = snapshot(ActiveIme::WeChatIme, Some(true));
        assert_eq!(
            plan(&s, TargetMode::ImeEnglish, ControlKind::Terminal, None),
            SwitchPlan::Apply
        );
        // Captured failure: Shift closes the context, but its NATIVE bit stays true.
        s.open = Some(false);
        for kind in [
            ControlKind::Terminal,
            ControlKind::AddressBar,
            ControlKind::Terminal,
        ] {
            for memory in [
                None,
                Some(TargetMode::ImeEnglish),
                Some(TargetMode::NativeChinese),
            ] {
                assert_eq!(
                    plan(&s, TargetMode::ImeEnglish, kind, memory),
                    SwitchPlan::Skip {
                        mode: TargetMode::ImeEnglish,
                        confirmed: false
                    }
                );
            }
        }
        // Manual Shift in another app changes the global state; old English memory is ignored.
        s.open = Some(true);
        assert_eq!(
            plan(
                &s,
                TargetMode::ImeEnglish,
                ControlKind::Terminal,
                Some(TargetMode::ImeEnglish)
            ),
            SwitchPlan::Apply
        );
        s.open = Some(false);
        assert_eq!(
            plan(
                &s,
                TargetMode::NativeChinese,
                ControlKind::Edit,
                Some(TargetMode::NativeChinese)
            ),
            SwitchPlan::Apply
        );
    }

    #[test]
    fn wechat_missing_live_evidence_never_uses_old_memory_to_toggle_or_skip() {
        let mut s = snapshot(ActiveIme::WeChatIme, None);
        for open in [None, Some(true)] {
            s.open = open;
            for target in [TargetMode::ImeEnglish, TargetMode::NativeChinese] {
                for memory in [
                    None,
                    Some(TargetMode::ImeEnglish),
                    Some(TargetMode::NativeChinese),
                ] {
                    assert_eq!(
                        plan(&s, target, ControlKind::Edit, memory),
                        SwitchPlan::Defer
                    );
                }
            }
        }
    }

    #[test]
    fn wechat_focus_restore_and_global_sync_restart_the_settling_window() {
        let start = Instant::now();
        let mut gate = WeChatSettling::default();
        let mut s = snapshot(ActiveIme::WeChatIme, Some(true));
        assert!(!gate.ready("terminal", 1, &s, start));
        assert!(!gate.ready("terminal", 1, &s, start + Duration::from_millis(120)));
        s.open = Some(false); // restored Chinese context synchronizes to global English
        assert!(!gate.ready("terminal", 1, &s, start + Duration::from_millis(240)));
        assert!(!gate.ready("terminal", 1, &s, start + Duration::from_millis(360)));
        assert!(gate.ready("terminal", 1, &s, start + Duration::from_millis(480)));
        assert!(!gate.ready("terminal", 2, &s, start + Duration::from_millis(600)));
        assert!(!gate.ready("address", 2, &s, start + Duration::from_millis(900)));
        assert!(gate.ready("address", 2, &s, start + Duration::from_millis(1140)));
        assert!(gate.ready(
            "address",
            2,
            &snapshot(ActiveIme::MsPinyin, Some(true)),
            start
        ));
        assert!(!gate.ready("address", 2, &s, start + Duration::from_millis(1500)));
    }

    #[test]
    fn final_recheck_does_not_toggle_a_context_that_synchronized_or_changed_backend() {
        let chinese = snapshot(ActiveIme::WeChatIme, Some(true));
        let mut english = chinese.clone();
        english.open = Some(false);
        assert_eq!(
            wechat_recheck(&chinese, &english, TargetMode::ImeEnglish),
            SwitchPlan::Defer
        );
        assert_eq!(
            wechat_recheck(&english, &english, TargetMode::ImeEnglish),
            SwitchPlan::Skip {
                mode: TargetMode::ImeEnglish,
                confirmed: false
            }
        );
        assert_eq!(
            wechat_recheck(&english, &english, TargetMode::NativeChinese),
            SwitchPlan::Apply
        );
        assert_eq!(
            wechat_recheck(&chinese, &chinese, TargetMode::ImeEnglish),
            SwitchPlan::Apply
        );
        assert_eq!(
            wechat_recheck(
                &chinese,
                &snapshot(ActiveIme::MsPinyin, Some(true)),
                TargetMode::ImeEnglish
            ),
            SwitchPlan::Defer
        );
        assert_eq!(
            wechat_recheck(
                &snapshot(ActiveIme::MsPinyin, Some(true)),
                &chinese,
                TargetMode::ImeEnglish
            ),
            SwitchPlan::Defer
        );
    }
}
