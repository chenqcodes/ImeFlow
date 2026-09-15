//! Microsoft Pinyin — primary supported backend. IMM readback treated as Confident.

use super::{plan_trusted, ImeBackend};
use crate::ime::{LayerObservation, TerminalEnPlan};
use crate::mode::TargetMode;

pub struct MsPinyinBackend;

impl ImeBackend for MsPinyinBackend {
    fn label(&self) -> &'static str {
        "MsPinyin"
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
        // Minimal: trust Confident only; no soft-memory Shift.
        plan_trusted(obs)
    }
}
