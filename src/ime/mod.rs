//! IME read/write. Per-IME quirks live in `backends/`; core stays MsPinyin-first.

mod backends;
mod layout;
mod conversion;
mod tsf;

use crate::mode::TargetMode;
use crate::uia::ControlKind;
use windows::Win32::Foundation::HWND;

pub use backends::{for_active, ImeBackend};
pub use layout::{classify_installed_imes, ActiveIme, LayoutInfo};
pub use conversion::{modifiers_down, tap_left_shift_scancode, tap_probe_letter_b, IMEF_EXTRA};

/// What we believe about the live 中/英 layer after reading OS signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerObservation {
    /// IMM/layout report is trustworthy enough to act on (e.g. MsPinyin).
    Confident(TargetMode),
    /// Weak signal (legacy WeChat IMM「英」). Core does not auto-switch on this alone.
    Probable(TargetMode),
    Unknown,
}

/// Terminal → 英 plan from the active backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalEnPlan {
    Skip,
    Shift,
    Defer,
}

pub fn plan_terminal_english(
    backend: ActiveIme,
    obs: LayerObservation,
    memory: Option<TargetMode>,
) -> TerminalEnPlan {
    for_active(backend).plan_terminal_english(obs, memory)
}

#[derive(Debug, Clone)]
pub struct ImeSnapshot {
    pub layout: LayoutInfo,
    pub conversion_native: Option<bool>,
    pub open: Option<bool>,
}

impl ImeSnapshot {
    pub fn describe(&self) -> String {
        let backend = self.layout.backend.label();
        match self.layout.backend {
            ActiveIme::LayoutEnglish => format!("{backend}/ENG"),
            ActiveIme::WeChatIme | ActiveIme::MsPinyin | ActiveIme::OtherChinese => {
                let mode = match self.conversion_native {
                    Some(true) => "中",
                    Some(false) => "英",
                    None => "?",
                };
                let closed = if self.open == Some(false) { "(closed)" } else { "" };
                format!("{backend}/{mode}{closed}")
            }
            ActiveIme::Unknown => format!("{backend}/{:04x}", self.layout.lang_id),
        }
    }

    pub fn as_target_guess(&self) -> TargetMode {
        match self.observe_layer() {
            LayerObservation::Confident(mode) | LayerObservation::Probable(mode) => mode,
            LayerObservation::Unknown => TargetMode::NativeChinese,
        }
    }

    pub fn observe_layer(&self) -> LayerObservation {
        if self.layout.backend == ActiveIme::LayoutEnglish {
            return LayerObservation::Confident(TargetMode::LayoutEnglish);
        }
        for_active(self.layout.backend).observe(self.conversion_native, self.open)
    }

    pub fn backend_label(&self) -> &'static str {
        if self.layout.backend == ActiveIme::LayoutEnglish {
            return ActiveIme::LayoutEnglish.label();
        }
        for_active(self.layout.backend).label()
    }

    pub fn auto_switch_enabled(&self) -> bool {
        matches!(self.layout.backend, ActiveIme::LayoutEnglish)
            || for_active(self.layout.backend).auto_switch_enabled()
    }

    /// Resolve live layer: Confident/Probable first, else optional memory fallback.
    pub fn resolve_layer(&self, memory: Option<TargetMode>) -> Option<TargetMode> {
        match self.observe_layer() {
            LayerObservation::Confident(mode) | LayerObservation::Probable(mode) => Some(mode),
            LayerObservation::Unknown => memory.filter(|m| {
                matches!(m, TargetMode::NativeChinese | TargetMode::ImeEnglish)
            }),
        }
    }
}

pub struct ImeController;

impl ImeController {
    pub fn read(hwnd: HWND) -> ImeSnapshot {
        let mut layout = layout::read_layout(hwnd);
        // HKL en-US wins — do not let a stale TSF tip overwrite real ENG layout.
        if layout.lang_id == 0x0409 {
            layout.backend = ActiveIme::LayoutEnglish;
        } else if let Some(tip) = tsf::active_keyboard_profile() {
            layout.backend = tip.backend;
            if tip.langid != 0 {
                layout.lang_id = tip.langid;
            }
        }
        let (open, conversion_native) = conversion::read_ime_status(hwnd);
        ImeSnapshot {
            layout,
            conversion_native,
            open,
        }
    }

    pub fn apply(hwnd: HWND, target: TargetMode) -> Result<ImeSnapshot, String> {
        Self::apply_for(hwnd, target, None)
    }

    pub fn apply_for(
        hwnd: HWND,
        target: TargetMode,
        kind: Option<ControlKind>,
    ) -> Result<ImeSnapshot, String> {
        if target == TargetMode::Passthrough {
            return Ok(Self::read(hwnd));
        }

        let input = crate::process::InputTarget::capture(hwnd)?;
        match target {
            TargetMode::LayoutEnglish => {
                input.ensure_current()?;
                layout::activate_english_layout(hwnd)?;
            }
            TargetMode::NativeChinese | TargetMode::ImeEnglish => {
                let native = target == TargetMode::NativeChinese;
                let before = Self::read(hwnd);
                if !before.auto_switch_enabled() {
                    return Err(format!(
                        "IME backend {} does not support auto-switch; use Microsoft Pinyin",
                        before.backend_label()
                    ));
                }
                let chinese = matches!(
                    before.layout.backend,
                    ActiveIme::WeChatIme | ActiveIme::MsPinyin | ActiveIme::OtherChinese
                );
                input.ensure_current()?;
                if kind == Some(ControlKind::Terminal) {
                    if !chinese {
                        return Err(
                            "terminal path needs a Chinese IME selected; layout unchanged".into(),
                        );
                    }
                    if native {
                        return Err("terminal path only pulses toward IME English".into());
                    }
                    tracing::info!("backend=terminal-focus-quiet-shift; keep active IME");
                    conversion::terminal_pulse_ime_english(hwnd)?;
                } else {
                    if !chinese {
                        layout::activate_chinese_ime(hwnd)?;
                        std::thread::sleep(std::time::Duration::from_millis(80));
                        input.ensure_current()?;
                    }
                    conversion::set_native_mode(hwnd, native)?;
                }
            }
            TargetMode::Passthrough => {}
        }
        std::thread::sleep(std::time::Duration::from_millis(80));
        if kind == Some(ControlKind::Terminal) {
            let fg = crate::process::foreground_hwnd();
            if fg.0.is_null() || crate::process::root_hwnd(fg) != crate::process::root_hwnd(hwnd) {
                return Err("terminal root lost foreground after IME action".into());
            }
        } else {
            input.ensure_current()?;
        }
        Ok(Self::read(hwnd))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ime::layout::LayoutInfo;

    fn snap(backend: ActiveIme, native: Option<bool>, open: Option<bool>) -> ImeSnapshot {
        ImeSnapshot {
            layout: LayoutInfo {
                lang_id: 0x0804,
                backend,
            },
            conversion_native: native,
            open,
        }
    }

    #[test]
    fn wechat_auto_switch_disabled() {
        let s = snap(ActiveIme::WeChatIme, Some(false), Some(true));
        assert!(!s.auto_switch_enabled());
        assert_eq!(
            s.observe_layer(),
            LayerObservation::Probable(TargetMode::ImeEnglish)
        );
        assert_eq!(
            plan_terminal_english(ActiveIme::WeChatIme, s.observe_layer(), None),
            TerminalEnPlan::Defer
        );
    }

    #[test]
    fn mspinyin_terminal_plan_is_minimal() {
        let s = snap(ActiveIme::MsPinyin, Some(true), Some(true));
        assert!(s.auto_switch_enabled());
        assert_eq!(
            s.observe_layer(),
            LayerObservation::Confident(TargetMode::NativeChinese)
        );
        assert_eq!(
            plan_terminal_english(ActiveIme::MsPinyin, s.observe_layer(), None),
            TerminalEnPlan::Shift
        );
        assert_eq!(
            plan_terminal_english(
                ActiveIme::MsPinyin,
                LayerObservation::Confident(TargetMode::ImeEnglish),
                Some(TargetMode::NativeChinese)
            ),
            TerminalEnPlan::Skip
        );
        // Unknown does not Shift from memory in MsPinyin core.
        assert_eq!(
            plan_terminal_english(
                ActiveIme::MsPinyin,
                LayerObservation::Unknown,
                Some(TargetMode::NativeChinese)
            ),
            TerminalEnPlan::Defer
        );
    }

    #[test]
    fn resolve_memory_only_when_unknown() {
        let s = snap(ActiveIme::MsPinyin, None, Some(true));
        assert_eq!(s.observe_layer(), LayerObservation::Unknown);
        assert_eq!(s.resolve_layer(None), None);
        assert_eq!(
            s.resolve_layer(Some(TargetMode::ImeEnglish)),
            Some(TargetMode::ImeEnglish)
        );
    }
}
