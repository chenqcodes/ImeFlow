//! Generic zh TIP (not WeChat / not MsPinyin). Same trust model as MsPinyin until proven otherwise.

use super::{plan_trusted, ImeBackend};
use crate::ime::{LayerObservation, TerminalEnPlan};
use crate::mode::TargetMode;

pub struct OtherChineseBackend;

impl ImeBackend for OtherChineseBackend {
    fn label(&self) -> &'static str {
        "ZhIME"
    }

    fn auto_switch_enabled(&self) -> bool {
        true
    }

    fn observe(&self, conversion_native: Option<bool>, _open: Option<bool>) -> LayerObservation {
        match conversion_native {
            Some(false) => LayerObservation::Confident(TargetMode::ImeEnglish),
            Some(true) => LayerObservation::Confident(TargetMode::NativeChinese),
            None => LayerObservation::Unknown,
        }
    }

    fn plan_terminal_english(
        &self,
        obs: LayerObservation,
        _memory: Option<TargetMode>,
    ) -> TerminalEnPlan {
        plan_trusted(obs)
    }
}
