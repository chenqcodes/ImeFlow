//! WeChat IME with “切换中英文对所有应用生效” enabled.
//! IMM is still only probable; callers must wait for stable readings before acting.

use super::ImeBackend;
use crate::ime::{LayerObservation, TerminalEnPlan};
use crate::mode::TargetMode;

pub struct WeChatBackend;

impl ImeBackend for WeChatBackend {
    fn label(&self) -> &'static str {
        "WeChatIME"
    }

    fn auto_switch_enabled(&self) -> bool {
        true
    }

    fn observe(&self, conversion_native: Option<bool>, open: Option<bool>) -> LayerObservation {
        // A closed context can retain NATIVE=true. Ignoring open turns every return to an
        // English terminal into another Shift. Missing signals must not use old app memory.
        match (open, conversion_native) {
            (Some(false), _) | (Some(true), Some(false)) => {
                LayerObservation::Probable(TargetMode::ImeEnglish)
            }
            (Some(true), Some(true)) => LayerObservation::Probable(TargetMode::NativeChinese),
            _ => LayerObservation::Unknown,
        }
    }

    fn plan_terminal_english(
        &self,
        obs: LayerObservation,
        _memory: Option<TargetMode>,
    ) -> TerminalEnPlan {
        match obs {
            LayerObservation::Probable(TargetMode::ImeEnglish) => TerminalEnPlan::Skip,
            LayerObservation::Probable(TargetMode::NativeChinese) => TerminalEnPlan::Shift,
            _ => TerminalEnPlan::Defer,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_context_is_english_even_with_stale_native_bit() {
        let backend = WeChatBackend;
        for native in [None, Some(false), Some(true)] {
            assert_eq!(
                backend.observe(native, Some(false)),
                LayerObservation::Probable(TargetMode::ImeEnglish)
            );
        }
        assert_eq!(
            backend.observe(Some(false), Some(true)),
            LayerObservation::Probable(TargetMode::ImeEnglish)
        );
        assert_eq!(
            backend.observe(Some(true), Some(true)),
            LayerObservation::Probable(TargetMode::NativeChinese)
        );
        assert_eq!(backend.observe(None, Some(true)), LayerObservation::Unknown);
        assert_eq!(backend.observe(Some(true), None), LayerObservation::Unknown);
        assert_eq!(
            backend.observe(Some(false), None),
            LayerObservation::Unknown
        );
    }
}
