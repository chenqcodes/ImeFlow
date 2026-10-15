use serde::{Deserialize, Serialize};
use std::fmt;

/// Product-level target modes (IME-backend agnostic).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetMode {
    /// Current Chinese IME in native/Chinese mode.
    NativeChinese,
    /// Same IME, alphanumeric / English mode (Shift layer).
    ImeEnglish,
    /// Real English keyboard layout (e.g. en-US).
    LayoutEnglish,
    /// Do nothing.
    Passthrough,
}

impl TargetMode {
    pub fn description(self) -> &'static str {
        match self { Self::NativeChinese => "中文", Self::ImeEnglish => "英文（当前输入法）", Self::LayoutEnglish => "ENG 美式键盘", Self::Passthrough => "不干预" }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::NativeChinese => "中",
            Self::ImeEnglish => "A",
            Self::LayoutEnglish => "ENG",
            Self::Passthrough => "—",
        }
    }

    /// 中 ↔ 英 style flips that cause tray flicker when focus hops.
    pub fn opposite_ime_layer(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::NativeChinese, Self::ImeEnglish)
                | (Self::ImeEnglish, Self::NativeChinese)
                | (Self::NativeChinese, Self::LayoutEnglish)
                | (Self::LayoutEnglish, Self::NativeChinese)
        )
    }
}

impl fmt::Display for TargetMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}
