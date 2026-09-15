//! Per-IME backends. Core only calls the trait; quirks live in each module.

mod mspinyin;
mod other;
mod wechat;

use crate::ime::{LayerObservation, TerminalEnPlan};
use crate::ime::layout::ActiveIme;
use crate::mode::TargetMode;

/// One IME family = one backend. Contrib adds a file here, registers in `for_active`.
pub trait ImeBackend: Send + Sync {
    fn label(&self) -> &'static str;
    /// false → core must not auto-write while this IME is active.
    fn auto_switch_enabled(&self) -> bool;
    fn observe(&self, conversion_native: Option<bool>, open: Option<bool>) -> LayerObservation;
    fn plan_terminal_english(
        &self,
        obs: LayerObservation,
        memory: Option<TargetMode>,
    ) -> TerminalEnPlan;
}

pub fn for_active(backend: ActiveIme) -> &'static dyn ImeBackend {
    match backend {
        ActiveIme::MsPinyin => &mspinyin::MsPinyinBackend,
        ActiveIme::OtherChinese => &other::OtherChineseBackend,
        ActiveIme::WeChatIme => &wechat::WeChatBackend,
        ActiveIme::LayoutEnglish | ActiveIme::Unknown => &mspinyin::MsPinyinBackend,
    }
}

/// Shared “IMM looks trustworthy” planner used by MsPinyin / OtherChinese.
pub(crate) fn plan_trusted(obs: LayerObservation) -> TerminalEnPlan {
    match obs {
        LayerObservation::Confident(TargetMode::ImeEnglish)
        | LayerObservation::Confident(TargetMode::LayoutEnglish) => TerminalEnPlan::Skip,
        LayerObservation::Confident(TargetMode::NativeChinese) => TerminalEnPlan::Shift,
        _ => TerminalEnPlan::Defer,
    }
}
