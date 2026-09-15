use crate::mode::TargetMode;
use crate::uia::ControlKind;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesFile {
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub process: String,
    pub mode: TargetMode,
    /// Optional: terminal / address-bar / edit / password / other
    #[serde(default)]
    pub control: Option<String>,
}

#[derive(Debug, Clone)]
struct CompiledRule {
    process: String,
    mode: TargetMode,
    control: Option<ControlKind>,
}

#[derive(Debug, Default)]
pub struct RuleEngine {
    rules: Vec<CompiledRule>,
}

impl RuleEngine {
    pub fn load_from(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        Self::from_text(&text).map_err(|e| format!("parse {}: {e}", path.display()))
    }

    fn from_text(text: &str) -> Result<Self, String> {
        let file: RulesFile = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut rules: Vec<CompiledRule> = Vec::new();
        for (index, r) in file.rules.into_iter().enumerate() {
            let process = r.process.trim().to_ascii_lowercase();
            if process.is_empty() || process.chars().any(|c| matches!(c, '/' | '\\' | '*' | '?' | ':')) {
                return Err(format!("rule {}: process must be an exact executable name, not a path/wildcard", index + 1));
            }
            let control = match r.control.as_deref() {
                None => None,
                Some(s) => Some(ControlKind::parse(s.trim()).ok_or_else(||
                    format!("rule {}: unknown control {s:?}", index + 1))?),
            };
            if rules.iter().any(|previous| previous.process == process && previous.control == control) {
                return Err(format!("rule {}: duplicate process/control key for {process}; keep exactly one", index + 1));
            }
            rules.push(CompiledRule { process, mode: r.mode, control });
        }
        // Explicit control rules retain precedence; process-only and password/terminal safety
        // fallbacks below are unchanged. Normalize names once, not on every matching iteration.
        rules.sort_by_key(|r| if r.control.is_some() { 0 } else { 1 });
        Ok(Self { rules })
    }

    /// Match process + optional focused control kind.
    ///
    /// - native-chinese / ime-english = 中文 IME 里的「中 / 英」(Shift)
    /// - layout-english = 真正的美式键盘（显式规则才用）
    /// - terminal/password 默认 passthrough（Imm 读回不可信）
    pub fn match_target(&self, exe_name: &str, control: Option<ControlKind>) -> TargetMode {
        let lower = exe_name.to_ascii_lowercase();

        // 1) Explicit process + control
        if let Some(got) = control {
            for r in &self.rules {
                if r.process == lower && r.control == Some(got) {
                    return r.mode;
                }
            }
        }

        // 2) Built-in: terminal / password → 不自动改 IME
        if matches!(
            control,
            Some(ControlKind::Terminal) | Some(ControlKind::Password)
        ) {
            return TargetMode::Passthrough;
        }

        // 3) Process-only fallback (edit / other / unknown control)
        for r in &self.rules {
            if r.process == lower && r.control.is_none() {
                return r.mode;
            }
        }
        TargetMode::Passthrough
    }

    /// Backward-compatible process-only match (no UIA).
    pub fn match_process(&self, exe_name: &str) -> TargetMode {
        self.match_target(exe_name, None)
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }
}

/// Resolve rules.toml: next to exe, then cwd, then repo-style relative.
pub fn resolve_rules_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("rules.toml");
            if p.exists() {
                return p;
            }
        }
    }
    let cwd = PathBuf::from("rules.toml");
    if cwd.exists() {
        return cwd;
    }
    PathBuf::from("rules.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn strict_rules_reject_typos_empty_names_and_duplicate_keys() {
        let valid = "[[rules]]\nprocess = ' Cursor.EXE '\ncontrol = ' edit '\nmode = 'native-chinese'\n";
        let engine = RuleEngine::from_text(valid).unwrap();
        assert_eq!(engine.match_target("cursor.exe", Some(ControlKind::Edit)), TargetMode::NativeChinese);
        assert!(RuleEngine::from_text(&format!("{valid}{valid}")).is_err());
        assert!(RuleEngine::from_text(&valid.replace(" Cursor.EXE ", "   ")).is_err());
        assert!(RuleEngine::from_text(&valid.replace(" Cursor.EXE ", "C:/Cursor.exe")).is_err());
        assert!(RuleEngine::from_text(&valid.replace("control =", "contorl =")).is_err());
        assert!(RuleEngine::from_text(&valid.replace("' edit '", "'unknown-control'")).is_err());
    }

    #[test]
    fn experiment_enables_only_named_integrated_terminals() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("rules.toml");
        let engine = RuleEngine::load_from(&path).unwrap();
        for exe in ["Cursor.exe", "Code.exe", "ChatGPT.exe", "Codex.exe"] {
            assert_eq!(engine.match_target(exe, Some(ControlKind::Terminal)), TargetMode::ImeEnglish);
            assert_eq!(engine.match_target(exe, Some(ControlKind::Password)), TargetMode::Passthrough);
            assert_eq!(engine.match_target(exe, Some(ControlKind::Edit)), TargetMode::NativeChinese);
        }
    }

    #[test]
    fn standalone_terminals_default_to_ime_english() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("rules.toml");
        let engine = RuleEngine::load_from(&path).unwrap();
        for exe in ["WindowsTerminal.exe", "cmd.exe", "powershell.exe", "pwsh.exe", "conhost.exe"] {
            assert_eq!(engine.match_target(exe, Some(ControlKind::Terminal)), TargetMode::ImeEnglish);
            assert_eq!(engine.match_target(exe, None), TargetMode::ImeEnglish);
        }
        assert_eq!(engine.match_target("unlisted.exe", Some(ControlKind::Terminal)), TargetMode::Passthrough);
    }

    #[test]
    fn browsers_use_english_address_bar_and_chinese_page_inputs() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("rules.toml");
        let engine = RuleEngine::load_from(&path).unwrap();
        for exe in ["msedge.exe", "chrome.exe", "firefox.exe"] {
            assert_eq!(engine.match_target(exe, Some(ControlKind::AddressBar)), TargetMode::ImeEnglish);
            assert_eq!(engine.match_target(exe, Some(ControlKind::Terminal)), TargetMode::ImeEnglish);
            assert_eq!(engine.match_target(exe, Some(ControlKind::Edit)), TargetMode::NativeChinese);
            assert_eq!(engine.match_target(exe, Some(ControlKind::Other)), TargetMode::Passthrough);
        }
    }

    #[test]
    fn control_rules_prefer_specific() {
        let mut path = std::env::temp_dir();
        path.push("imeflow_rules_control_test.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"
[[rules]]
process = "Cursor.exe"
control = "terminal"
mode = "ime-english"

[[rules]]
process = "Cursor.exe"
control = "edit"
mode = "native-chinese"

[[rules]]
process = "Cursor.exe"
mode = "ime-english"

[[rules]]
process = "WeChat.exe"
mode = "native-chinese"
"#
        )
        .unwrap();
        let engine = RuleEngine::load_from(&path).unwrap();
        assert_eq!(
            engine.match_target("Cursor.exe", Some(ControlKind::Terminal)),
            TargetMode::ImeEnglish
        );
        assert_eq!(
            engine.match_target("Cursor.exe", Some(ControlKind::Edit)),
            TargetMode::NativeChinese
        );
        assert_eq!(
            engine.match_target("Cursor.exe", Some(ControlKind::Other)),
            TargetMode::ImeEnglish
        );
        assert_eq!(
            engine.match_process("WeChat.exe"),
            TargetMode::NativeChinese
        );
        // No explicit terminal rule → built-in passthrough (don't fight Imm-untrusted hosts)
        assert_eq!(
            engine.match_target("WeChat.exe", Some(ControlKind::Terminal)),
            TargetMode::Passthrough
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn chatgpt_terminal_not_swallowed_by_chinese_process_rule() {
        let mut path = std::env::temp_dir();
        path.push("imeflow_rules_chatgpt_term.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"
[[rules]]
process = "ChatGPT.exe"
mode = "native-chinese"
"#
        )
        .unwrap();
        let engine = RuleEngine::load_from(&path).unwrap();
        assert_eq!(
            engine.match_target("ChatGPT.exe", Some(ControlKind::Edit)),
            TargetMode::NativeChinese
        );
        assert_eq!(
            engine.match_target("ChatGPT.exe", Some(ControlKind::Terminal)),
            TargetMode::Passthrough
        );
        let _ = std::fs::remove_file(path);
    }
}
