//! WeChat IME — not auto-switched in core (IMM readback unreliable).
//!
//! Pitfalls archived locally: `.local/notes/pitfalls.md` (gitignored).
//! Future contrib: enable `auto_switch_enabled` only with a strategy that does not
//! pollute `main` / MsPinyin paths (Probable / soft memory / LL override, etc.).

use super::ImeBackend;
use crate::ime::{LayerObservation, TerminalEnPlan};
use crate::mode::TargetMode;

pub struct WeChatBackend;

impl ImeBackend for WeChatBackend {
    fn label(&self) -> &'static str {
        "WeChatIME"
    }

    fn auto_switch_enabled(&self) -> bool {
        false
    }

    fn observe(&self, conversion_native: Option<bool>, _open: Option<bool>) -> LayerObservation {
        // Kept for diagnostics / future backend work. Core ignores writes while disabled.
        match conversion_native {
            Some(false) => LayerObservation::Probable(TargetMode::ImeEnglish),
            _ => LayerObservation::Unknown,
        }
    }

    fn plan_terminal_english(
        &self,
        _obs: LayerObservation,
        _memory: Option<TargetMode>,
    ) -> TerminalEnPlan {
        TerminalEnPlan::Defer
    }
}
