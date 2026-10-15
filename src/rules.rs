use crate::mode::TargetMode;
use crate::uia::{ControlKind, FocusInfo};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct RuleStore {
    path: PathBuf,
    engine: parking_lot::RwLock<Arc<RuleEngine>>,
    write_lock: parking_lot::Mutex<()>,
}
impl RuleStore {
    pub fn load(path: &Path) -> Result<Self, String> {
        let path = std::fs::canonicalize(path)
            .map_err(|e| format!("无法打开规则文件 {}：{e}", path.display()))?;
        let engine = RuleEngine::load_from(&path)?;
        Ok(Self {
            path,
            engine: parking_lot::RwLock::new(Arc::new(engine)),
            write_lock: parking_lot::Mutex::new(()),
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn snapshot(&self) -> Arc<RuleEngine> {
        Arc::clone(&self.engine.read())
    }
    pub fn reload(&self) -> Result<usize, String> {
        let _guard = self.write_lock.lock();
        // Validate completely before replacement; callbacks retain a consistent old snapshot.
        let next = RuleEngine::load_from(&self.path)?;
        let count = next.len();
        *self.engine.write() = Arc::new(next);
        Ok(count)
    }

    /// Preserve comments and unrelated rules; publish only after the file is safely replaced.
    /// A stale panel must never overwrite a concurrent manual edit.
    pub fn save(
        &self,
        expected: &str,
        scope: &RuleScope,
        mode: Option<TargetMode>,
    ) -> Result<(), String> {
        let text = edit_rule(expected, scope, mode)?;
        self.replace(expected, &text)
    }

    /// Shared atomic path for editor, validated import, presets and backup restoration.
    pub fn replace(&self, expected: &str, text: &str) -> Result<(), String> {
        use std::io::Write;
        let _guard = self.write_lock.lock();
        let original = std::fs::read_to_string(&self.path).map_err(|e| e.to_string())?;
        if original != expected {
            return Err("配置文件已被其他操作修改，请重新打开设置或重新点选后再保存。".into());
        }
        let engine = RuleEngine::from_text(text)?;
        let temp = self
            .path
            .with_extension(format!("toml.{}.tmp", std::process::id()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| format!("无法创建临时配置：{e}"))?;
        let result = (|| {
            file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            drop(file);
            if std::fs::read_to_string(&self.path).map_err(|e| e.to_string())? != original {
                return Err("配置文件已改变，本次未保存，请重新打开设置。".into());
            }
            std::fs::write(self.path.with_extension("toml.bak"), original.as_bytes())
                .map_err(|e| format!("无法备份配置：{e}"))?;
            // Same-directory rename replaces the destination atomically on Windows.
            std::fs::rename(&temp, &self.path).map_err(|e| format!("无法替换配置：{e}"))?;
            *self.engine.write() = Arc::new(engine);
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }

    pub fn read_text(&self) -> Result<String, String> {
        std::fs::read_to_string(&self.path).map_err(|e| e.to_string())
    }

    pub fn backup_text(&self) -> Result<String, String> {
        let text = std::fs::read_to_string(self.path.with_extension("toml.bak"))
            .map_err(|e| format!("无法读取上一份配置：{e}"))?;
        RuleEngine::from_text(&text)?;
        Ok(text)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElementSelector {
    pub automation_id: String,
    pub class_name: String,
    pub control_type: i32,
}

impl ElementSelector {
    pub fn from_focus(focus: &FocusInfo) -> Option<Self> {
        if focus.automation_id.trim().is_empty() || focus.control_type <= 0 {
            return None;
        }
        Some(Self {
            automation_id: focus.automation_id.clone(),
            class_name: focus.class_name.clone(),
            control_type: focus.control_type,
        })
    }

    fn matches(&self, focus: &FocusInfo) -> bool {
        self.automation_id == focus.automation_id
            && self.class_name == focus.class_name
            && self.control_type == focus.control_type
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleScope {
    Global(ControlKind),
    Application {
        process: String,
        control: Option<ControlKind>,
        element: Option<ElementSelector>,
    },
}

impl RuleScope {
    pub fn label(&self) -> String {
        match self {
            Self::Global(kind) => format!("所有软件 / {}", kind.label()),
            Self::Application {
                process,
                control,
                element,
            } => {
                let region = control.map(ControlKind::label).unwrap_or("软件默认");
                if let Some(element) = element {
                    format!("{process} / {region} / 标识 {}", element.automation_id)
                } else {
                    format!("{process} / {region}")
                }
            }
        }
    }
    pub fn process(&self) -> Option<&str> {
        match self {
            Self::Global(_) => None,
            Self::Application { process, .. } => Some(process),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuleEntry {
    pub scope: RuleScope,
    pub mode: TargetMode,
    pub configured: bool,
}

struct RuleMatch {
    mode: TargetMode,
    source: &'static str,
    scope: Option<RuleScope>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesFile {
    /// Missing terminal/password entries retain the legacy protected fallback.
    pub defaults: Option<BTreeMap<String, TargetMode>>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub process: String,
    pub mode: TargetMode,
    /// Optional input-region type.
    #[serde(default)]
    pub control: Option<String>,
    #[serde(default)]
    pub element: Option<ElementSelector>,
}

#[derive(Debug, Clone)]
struct CompiledRule {
    process: String,
    mode: TargetMode,
    control: Option<ControlKind>,
    element: Option<ElementSelector>,
}

#[derive(Debug, Default)]
pub struct RuleEngine {
    rules: Vec<CompiledRule>,
    defaults: Option<BTreeMap<String, TargetMode>>,
}

impl RuleEngine {
    pub fn load_from(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("无法读取 {}：{e}", path.display()))?;
        Self::from_text(&text).map_err(|e| format!("{}\n{e}", path.display()))
    }

    pub fn from_text(text: &str) -> Result<Self, String> {
        let value: toml::Value = toml::from_str(text).map_err(|e: toml::de::Error| {
            let line = e
                .span()
                .map(|span| text[..span.start].bytes().filter(|b| *b == b'\n').count() + 1);
            format!(
                "TOML 格式错误（第 {} 行）：{}",
                line.unwrap_or(1),
                e.message()
            )
        })?;
        validate_fields(&value)?;
        let file: RulesFile = value.try_into().map_err(|e: toml::de::Error| {
            format!(
                "规则格式错误：{}。每条规则需要 process 和 mode；使用 [[rules]] 分隔。",
                e.message()
            )
        })?;
        let mut rules: Vec<CompiledRule> = Vec::new();
        if let Some(defaults) = &file.defaults {
            for key in defaults.keys() {
                if !ControlKind::ALL.iter().any(|kind| kind.as_str() == key) {
                    return Err(format!("未知全局输入区域 {key:?}；可用 edit、terminal、address-bar、password、code-editor、other。"));
                }
            }
        }
        for (index, r) in file.rules.into_iter().enumerate() {
            let process = r.process.trim().to_ascii_lowercase();
            if process.is_empty()
                || process
                    .chars()
                    .any(|c| matches!(c, '/' | '\\' | '*' | '?' | ':'))
            {
                return Err(format!("第 {} 条规则：process 必须是完整进程名，例如 Cursor.exe；不能留空、填写路径或使用通配符。", index + 1));
            }
            let control = match r.control.as_deref() {
                None => None,
                Some(s) => Some(ControlKind::parse(s.trim()).ok_or_else(||
                    format!("第 {} 条规则：未知输入区域 {s:?}。control 可用 terminal、address-bar、edit、password、code-editor、other；整个应用的默认规则请省略 control。", index + 1))?),
            };
            if let Some(element) = &r.element {
                if element.automation_id.trim().is_empty()
                    || element.control_type <= 0
                    || control.is_none()
                {
                    return Err(format!("第 {} 条规则：控件规则需要 control、非空 automation_id 和有效 control_type。", index + 1));
                }
            }
            if rules.iter().any(|previous| {
                previous.process == process
                    && previous.control == control
                    && previous.element == r.element
            }) {
                return Err(format!(
                    "第 {} 条规则：{process} / {} 重复配置，请保留一条；需要修改时编辑已有规则。",
                    index + 1,
                    control.map(|c| c.as_str()).unwrap_or("应用默认")
                ));
            }
            rules.push(CompiledRule {
                process,
                mode: r.mode,
                control,
                element: r.element,
            });
        }
        // Explicit control rules retain precedence; process-only and password/terminal safety
        // fallbacks below are unchanged. Normalize names once, not on every matching iteration.
        rules.sort_by_key(|r| if r.control.is_some() { 0 } else { 1 });
        Ok(Self {
            rules,
            defaults: file.defaults,
        })
    }

    /// Match process + optional focused control kind.
    ///
    /// - native-chinese / ime-english = 中文 IME 里的「中 / 英」(Shift)
    /// - layout-english = 真正的美式键盘（显式规则才用）
    /// - Old files without terminal/password defaults retain their passthrough fallback.
    pub fn match_target(&self, exe_name: &str, control: Option<ControlKind>) -> TargetMode {
        self.resolve(exe_name, control, None).0
    }

    pub fn match_focus(
        &self,
        exe_name: &str,
        focus: &FocusInfo,
        effective_kind: ControlKind,
    ) -> TargetMode {
        self.resolve(exe_name, Some(effective_kind), Some(focus)).0
    }

    pub fn resolve(
        &self,
        exe_name: &str,
        control: Option<ControlKind>,
        focus: Option<&FocusInfo>,
    ) -> (TargetMode, &'static str) {
        let matched = self.resolve_detail(exe_name, control, focus);
        (matched.mode, matched.source)
    }

    fn resolve_detail(
        &self,
        exe_name: &str,
        control: Option<ControlKind>,
        focus: Option<&FocusInfo>,
    ) -> RuleMatch {
        if focus.is_some_and(FocusInfo::is_navigation_control) {
            return RuleMatch {
                mode: TargetMode::Passthrough,
                source: "非输入控件，不执行规则",
                scope: None,
            };
        }
        let lower = exe_name.to_ascii_lowercase();

        if let Some(focus) = focus {
            for r in &self.rules {
                if r.process == lower
                    && r.control == Some(focus.kind)
                    && r.element.as_ref().is_some_and(|e| e.matches(focus))
                {
                    return r.matched("此软件的控件标识规则");
                }
            }
        }

        // 1) Explicit process + control
        if let Some(got) = control {
            for r in &self.rules {
                if r.process == lower && r.control == Some(got) && r.element.is_none() {
                    return r.matched("此软件的区域规则");
                }
            }
        }

        // 2) Built-in: terminal / password → 不自动改 IME
        if !control.is_some_and(|kind| {
            self.defaults
                .as_ref()
                .is_some_and(|d| d.contains_key(kind.as_str()))
        }) && matches!(
            control,
            Some(ControlKind::Terminal) | Some(ControlKind::Password)
        ) {
            return RuleMatch {
                mode: TargetMode::Passthrough,
                source: "旧配置的终端 / 密码框默认",
                scope: None,
            };
        }

        // 3) Process-only fallback (edit / other / unknown control)
        for r in &self.rules {
            if r.process == lower && r.control.is_none() && r.element.is_none() {
                return r.matched("此软件的默认规则");
            }
        }
        RuleMatch {
            mode: self.global_mode(control),
            source: "通用默认",
            scope: control.map(RuleScope::Global),
        }
    }

    pub fn entries(&self) -> Vec<RuleEntry> {
        let mut entries: Vec<_> = ControlKind::ALL
            .into_iter()
            .map(|kind| RuleEntry {
                scope: RuleScope::Global(kind),
                mode: self.global_mode(Some(kind)),
                configured: self
                    .defaults
                    .as_ref()
                    .is_some_and(|d| d.contains_key(kind.as_str())),
            })
            .collect();
        entries.extend(self.rules.iter().map(|r| RuleEntry {
            scope: r.scope(),
            mode: r.mode,
            configured: true,
        }));
        entries
    }

    /// Shows exact configurations, not inherited values disguised as explicit rules.
    pub fn related_configuration(&self, process: &str, focus: &FocusInfo) -> String {
        let matched = self.resolve_detail(process, Some(focus.kind), Some(focus));
        let entries = self.entries();
        let app = |control, element| RuleScope::Application {
            process: process.to_ascii_lowercase(),
            control,
            element,
        };
        let selector = ElementSelector::from_focus(focus);
        let mut layers = Vec::new();
        if let Some(selector) = selector {
            layers.push(("01  此控件", app(Some(focus.kind), Some(selector))));
        }
        layers.push(("02  此软件 · 同类区域", app(Some(focus.kind), None)));
        layers.push(("03  此软件 · 默认", app(None, None)));
        layers.push(("04  所有软件 · 同类区域", RuleScope::Global(focus.kind)));
        let mut lines = vec![format!(
            "{} · {}\r\n当前目标：{}\r\n来源：{}",
            process,
            focus.kind.label(),
            matched.mode.description(),
            matched.source
        )];
        if ElementSelector::from_focus(focus).is_none() {
            lines.push("01  此控件\r\n无可匹配标识，未使用控件规则".into());
        }
        for (label, scope) in layers {
            let entry = entries.iter().find(|e| e.scope == scope);
            let value = match entry {
                Some(e) if e.configured => format!("{} · 已配置", e.mode.description()),
                Some(e) => format!("{} · 未配置，使用兜底", e.mode.description()),
                None => "未配置 · 继续使用下一层".into(),
            };
            let state = if matched.scope.as_ref() == Some(&scope) {
                "  [当前命中]"
            } else {
                ""
            };
            lines.push(format!("{label}{state}\r\n{value}"));
        }
        lines.push("规则目标不等于输入法实际状态。".into());
        lines.join("\r\n\r\n")
    }

    pub fn explain(&self, process: &str, focus: &FocusInfo) -> String {
        let selected = self.resolve_detail(process, Some(focus.kind), Some(focus));
        let mut lines = vec![
            format!("软件：{process}"),
            format!("识别区域：{}", focus.kind.label()),
            format!("匹配目标：{}", selected.mode.description()),
            format!("来源：{}", selected.source),
        ];
        if let Some(scope) = selected.scope {
            lines.push(format!("命中规则：{}", scope.label()));
        }
        if let Some(selector) = ElementSelector::from_focus(focus) {
            lines.push(format!(
                "控件标识：{}\r\n控件类型：{}；类名：{}",
                selector.automation_id, selector.control_type, selector.class_name
            ));
        }
        lines.push("\r\n优先级：控件标识 > 软件区域 > 软件默认 > 通用默认。".into());
        lines.push("更具体的规则覆盖通用默认；不干预也参与此优先级。".into());
        lines.push("\r\n以下是该软件相关的区域 / 默认规则：".into());
        for entry in self.entries().into_iter().filter(|e| match &e.scope {
            RuleScope::Global(kind) => *kind == focus.kind,
            RuleScope::Application {
                process: p,
                control,
                element,
            } => {
                p.eq_ignore_ascii_case(process)
                    && element.is_none()
                    && (control.is_none() || *control == Some(focus.kind))
            }
        }) {
            lines.push(format!(
                "{} → {}{}",
                entry.scope.label(),
                entry.mode.description(),
                if entry.configured {
                    ""
                } else {
                    "（未配置，使用兜底）"
                }
            ));
        }
        lines.push("\r\n这是根据捕获区域试算的规则目标，不代表输入法已切换成功。".into());
        lines.join("\r\n")
    }

    pub fn global_mode(&self, control: Option<ControlKind>) -> TargetMode {
        control
            .and_then(|kind| self.defaults.as_ref()?.get(kind.as_str()).copied())
            .unwrap_or(TargetMode::Passthrough)
    }

    /// Backward-compatible process-only match (no UIA).
    #[cfg(test)]
    pub fn match_process(&self, exe_name: &str) -> TargetMode {
        self.match_target(exe_name, None)
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }
}

impl CompiledRule {
    fn scope(&self) -> RuleScope {
        RuleScope::Application {
            process: self.process.clone(),
            control: self.control,
            element: self.element.clone(),
        }
    }
    fn matched(&self, source: &'static str) -> RuleMatch {
        RuleMatch {
            mode: self.mode,
            source,
            scope: Some(self.scope()),
        }
    }
}

/// Add only missing scopes. Applying a preset must not overwrite a user's settings.
pub fn apply_preset(text: &str, preset: usize) -> Result<(String, usize), String> {
    let scopes = preset_entries(preset)?;
    let mut result = text.to_owned();
    let mut added = 0;
    for (scope, mode) in scopes {
        let existing = RuleEngine::from_text(&result)?
            .entries()
            .into_iter()
            .any(|e| e.configured && e.scope == scope);
        if !existing {
            result = edit_rule(&result, &scope, Some(mode))?;
            added += 1;
        }
    }
    Ok((result, added))
}

pub const PRESETS: [&str; 3] = [
    "浏览器：地址栏 / 终端英文，输入框中文",
    "开发工具：代码 / 终端英文，输入框中文",
    "聊天与记事本：未分类区域中文",
];

fn preset_entries(preset: usize) -> Result<Vec<(RuleScope, TargetMode)>, String> {
    let (processes, regions): (&[&str], &[(ControlKind, TargetMode)]) = match preset {
        0 => (
            &["msedge.exe", "chrome.exe", "firefox.exe"],
            &[
                (ControlKind::AddressBar, TargetMode::ImeEnglish),
                (ControlKind::Terminal, TargetMode::ImeEnglish),
                (ControlKind::Edit, TargetMode::NativeChinese),
            ],
        ),
        1 => (
            &["cursor.exe", "code.exe"],
            &[
                (ControlKind::CodeEditor, TargetMode::ImeEnglish),
                (ControlKind::Terminal, TargetMode::ImeEnglish),
                (ControlKind::Edit, TargetMode::NativeChinese),
            ],
        ),
        2 => (
            &["wechat.exe", "weixin.exe", "chatgpt.exe", "notepad.exe"],
            &[(ControlKind::Other, TargetMode::NativeChinese)],
        ),
        _ => return Err("未知预设".into()),
    };
    Ok(processes
        .iter()
        .flat_map(|process| {
            regions.iter().map(move |(kind, mode)| {
                (
                    RuleScope::Application {
                        process: (*process).into(),
                        control: Some(*kind),
                        element: None,
                    },
                    *mode,
                )
            })
        })
        .collect())
}

fn edit_rule(text: &str, scope: &RuleScope, mode: Option<TargetMode>) -> Result<String, String> {
    use toml_edit::{value, ArrayOfTables, DocumentMut, Item, Table};
    RuleEngine::from_text(text)?;
    let mut document = text.parse::<DocumentMut>().map_err(|e| e.to_string())?;
    let mode_value = |mode: TargetMode| {
        value(match mode {
            TargetMode::NativeChinese => "native-chinese",
            TargetMode::ImeEnglish => "ime-english",
            TargetMode::LayoutEnglish => "layout-english",
            TargetMode::Passthrough => "passthrough",
        })
    };
    match scope {
        RuleScope::Global(kind) => {
            if document.get("defaults").is_none() {
                document["defaults"] = Item::Table(Table::new());
            }
            let table = document["defaults"]
                .as_table_mut()
                .ok_or("defaults 必须为表格")?;
            if let Some(mode) = mode {
                table[kind.as_str()] = mode_value(mode);
            } else {
                table.remove(kind.as_str());
            }
        }
        RuleScope::Application {
            process,
            control,
            element,
        } => {
            if document.get("rules").is_none() {
                document["rules"] = Item::ArrayOfTables(ArrayOfTables::new());
            }
            let rules = document["rules"]
                .as_array_of_tables_mut()
                .ok_or("rules 必须为规则列表")?;
            let found = rules.iter().position(|table| {
                let process_match = table
                    .get("process")
                    .and_then(Item::as_str)
                    .is_some_and(|s| s.trim().eq_ignore_ascii_case(process));
                let kind = table
                    .get("control")
                    .and_then(Item::as_str)
                    .and_then(|s| ControlKind::parse(s.trim()));
                let saved_element =
                    table
                        .get("element")
                        .and_then(Item::as_table_like)
                        .and_then(|t| {
                            Some(ElementSelector {
                                automation_id: t.get("automation_id")?.as_str()?.into(),
                                class_name: t.get("class_name")?.as_str()?.into(),
                                control_type: i32::try_from(t.get("control_type")?.as_integer()?)
                                    .ok()?,
                            })
                        });
                process_match && kind == *control && saved_element == *element
            });
            if let Some(mode) = mode {
                if let Some(index) = found {
                    rules.get_mut(index).unwrap()["mode"] = mode_value(mode);
                } else {
                    let mut table = Table::new();
                    table["process"] = value(process.as_str());
                    if let Some(kind) = control {
                        table["control"] = value(kind.as_str());
                    }
                    table["mode"] = mode_value(mode);
                    if let Some(element) = element {
                        let mut fields = toml_edit::InlineTable::new();
                        fields.insert("automation_id", element.automation_id.as_str().into());
                        fields.insert("class_name", element.class_name.as_str().into());
                        fields.insert("control_type", (element.control_type as i64).into());
                        table["element"] = value(fields);
                    }
                    rules.push(table);
                }
            } else if let Some(index) = found {
                rules.remove(index);
            }
        }
    }
    let result = document.to_string();
    RuleEngine::from_text(&result)?;
    Ok(result)
}

fn validate_fields(value: &toml::Value) -> Result<(), String> {
    let root = value.as_table().ok_or("规则文件需要使用 [[rules]] 表格")?;
    for key in root.keys() {
        if key != "rules" && key != "defaults" {
            return Err(format!(
                "未知顶层字段 {key:?}；请使用 [defaults] 或 [[rules]]。"
            ));
        }
    }
    if let Some(items) = root.get("rules") {
        let items = items
            .as_array()
            .ok_or("rules 必须是规则列表；请使用 [[rules]]，不要使用 [rules]。")?;
        for (index, item) in items.iter().enumerate() {
            let table = item.as_table().ok_or("每条规则必须使用 [[rules]] 表格")?;
            for key in table.keys() {
                if !["process", "control", "mode", "element"].contains(&key.as_str()) {
                    let hint = if key.starts_with("cont") {
                        "；是否想填写 control？"
                    } else {
                        "。"
                    };
                    return Err(format!(
                        "第 {} 条规则：未知字段 {key:?}{hint}允许的字段为 process、control、mode、element。",
                        index + 1
                    ));
                }
            }
            for key in ["process", "mode"] {
                if !table.contains_key(key) {
                    return Err(format!("第 {} 条规则：缺少 {key}。", index + 1));
                }
            }
            if let Some(mode) = table.get("mode").and_then(|v| v.as_str()) {
                if ![
                    "native-chinese",
                    "ime-english",
                    "layout-english",
                    "passthrough",
                ]
                .contains(&mode)
                {
                    return Err(format!("第 {} 条规则：未知模式 {mode:?}。mode 可用 native-chinese（中文）、ime-english（英文）、layout-english（ENG）、passthrough（不干预）。", index + 1));
                }
            }
        }
    }
    Ok(())
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
    fn presets_are_idempotent_and_preserve_exact_overrides() {
        let text =
            "# custom\n[[rules]]\nprocess='CHROME.EXE'\ncontrol='edit'\nmode='passthrough'\n";
        let (next, count) = apply_preset(text, 0).unwrap();
        assert_eq!(count, 8);
        assert!(next.contains("# custom"));
        assert_eq!(
            RuleEngine::from_text(&next)
                .unwrap()
                .match_target("chrome.exe", Some(ControlKind::Edit)),
            TargetMode::Passthrough
        );
        let (again, count) = apply_preset(&next, 0).unwrap();
        assert_eq!(count, 0);
        assert_eq!(next, again);
    }

    #[test]
    fn related_configuration_distinguishes_explicit_inherited_and_matched_layers() {
        let source = "[defaults]\nedit='native-chinese'\n[[rules]]\nprocess='demo.exe'\nmode='ime-english'\n";
        let focus = FocusInfo {
            kind: ControlKind::Edit,
            ..Default::default()
        };
        let details = RuleEngine::from_text(source)
            .unwrap()
            .related_configuration("DEMO.EXE", &focus);
        assert!(details.contains("02  此软件 · 同类区域\r\n未配置"));
        assert!(details.contains("03  此软件 · 默认  [当前命中]"));
        assert!(details.contains("04  所有软件 · 同类区域\r\n中文 · 已配置"));
        assert_eq!(details.matches("[当前命中]").count(), 1);
        let legacy =
            RuleEngine::from_text("[[rules]]\nprocess='demo.exe'\nmode='native-chinese'").unwrap();
        let terminal = FocusInfo {
            kind: ControlKind::Terminal,
            ..Default::default()
        };
        let details = legacy.related_configuration("demo.exe", &terminal);
        assert!(details.contains("旧配置的终端 / 密码框默认"));
        assert!(!details.contains("[当前命中]"));
    }

    #[test]
    fn replacement_and_restore_preserve_live_state_on_validation_failure() {
        let path =
            std::env::temp_dir().join(format!("imeflow-restore-{}.toml", std::process::id()));
        let original = "[defaults]\nedit='native-chinese'\n";
        std::fs::write(&path, original).unwrap();
        let store = RuleStore::load(&path).unwrap();
        assert!(store.replace(original, "[defaults]\nedit='typo'").is_err());
        assert_eq!(store.read_text().unwrap(), original);
        assert_eq!(
            store.snapshot().global_mode(Some(ControlKind::Edit)),
            TargetMode::NativeChinese
        );
        let next = "[defaults]\nedit='ime-english'\n";
        store.replace(original, next).unwrap();
        assert_eq!(store.backup_text().unwrap(), original);
        store.replace(next, &store.backup_text().unwrap()).unwrap();
        assert_eq!(store.read_text().unwrap(), original);
        assert_eq!(store.backup_text().unwrap(), next);
        assert_eq!(
            store.snapshot().global_mode(Some(ControlKind::Edit)),
            TargetMode::NativeChinese
        );
        std::fs::write(store.path().with_extension("toml.bak"), "invalid").unwrap();
        assert!(store.backup_text().is_err());
        assert_eq!(store.read_text().unwrap(), original);
        std::fs::remove_file(store.path().with_extension("toml.bak")).unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn manual_overrides_and_removal_follow_precedence() {
        let base = "# Keep this comment\n[defaults]\nedit = 'native-chinese'\npassword = 'ime-english'\nother = 'passthrough'\n";
        let focus = FocusInfo {
            kind: ControlKind::Edit,
            automation_id: "compose".into(),
            class_name: "TextBox".into(),
            control_type: 50004,
            ..Default::default()
        };
        let app = RuleScope::Application {
            process: "Demo.exe".into(),
            control: None,
            element: None,
        };
        let region = RuleScope::Application {
            process: "Demo.exe".into(),
            control: Some(ControlKind::Edit),
            element: None,
        };
        let element = RuleScope::Application {
            process: "Demo.exe".into(),
            control: Some(ControlKind::Edit),
            element: ElementSelector::from_focus(&focus),
        };
        let text = edit_rule(base, &app, Some(TargetMode::ImeEnglish)).unwrap();
        let text = edit_rule(&text, &region, Some(TargetMode::NativeChinese)).unwrap();
        let text = edit_rule(&text, &element, Some(TargetMode::Passthrough)).unwrap();
        assert!(text.contains("# Keep this comment"));
        let engine = RuleEngine::from_text(&text).unwrap();
        let explanation = engine.explain("DEMO.EXE", &focus);
        assert!(explanation.contains("来源：此软件的控件标识规则"));
        assert!(explanation.contains("匹配目标：不干预"));
        assert_eq!(
            engine.match_focus("DEMO.EXE", &focus, focus.kind),
            TargetMode::Passthrough
        );
        let different = FocusInfo {
            automation_id: "search".into(),
            ..focus.clone()
        };
        assert_eq!(
            engine.match_focus("Demo.exe", &different, different.kind),
            TargetMode::NativeChinese
        );
        assert_eq!(
            engine.match_target("Demo.exe", Some(ControlKind::Password)),
            TargetMode::ImeEnglish
        );
        let text = edit_rule(&text, &element, None).unwrap();
        assert_eq!(
            RuleEngine::from_text(&text)
                .unwrap()
                .match_focus("Demo.exe", &focus, focus.kind),
            TargetMode::NativeChinese
        );
        let text = edit_rule(&text, &region, None).unwrap();
        assert_eq!(
            RuleEngine::from_text(&text)
                .unwrap()
                .match_focus("Demo.exe", &focus, focus.kind),
            TargetMode::ImeEnglish
        );
        let text = edit_rule(&text, &app, None).unwrap();
        assert_eq!(
            RuleEngine::from_text(&text)
                .unwrap()
                .match_focus("Demo.exe", &focus, focus.kind),
            TargetMode::NativeChinese
        );
    }

    #[test]
    fn password_global_can_be_overridden_and_unknown_stays_untouched() {
        let base = include_str!("../rules.toml");
        let scope = RuleScope::Application {
            process: "app.exe".into(),
            control: Some(ControlKind::Password),
            element: None,
        };
        let text = edit_rule(base, &scope, Some(TargetMode::Passthrough)).unwrap();
        let engine = RuleEngine::from_text(&text).unwrap();
        assert_eq!(
            engine.match_target("app.exe", Some(ControlKind::Password)),
            TargetMode::Passthrough
        );
        assert_eq!(
            engine.match_target("other.exe", Some(ControlKind::Password)),
            TargetMode::ImeEnglish
        );
        assert_eq!(
            engine.match_target("other.exe", Some(ControlKind::Other)),
            TargetMode::Passthrough
        );
        assert_eq!(
            engine.match_target("other.exe", None),
            TargetMode::Passthrough
        );
        let nav = FocusInfo {
            kind: ControlKind::Edit,
            control_type: 50000,
            aria_role: "button".into(),
            ..Default::default()
        };
        assert_eq!(
            engine.match_focus("app.exe", &nav, nav.kind),
            TargetMode::Passthrough
        );
    }

    #[test]
    fn editor_updates_existing_alias_rule_without_duplicates() {
        let text = "# untouched\n[[rules]]\nprocess = ' APP.EXE '\ncontrol = ' input '\nmode = 'native-chinese'\n";
        let scope = RuleScope::Application {
            process: "app.exe".into(),
            control: Some(ControlKind::Edit),
            element: None,
        };
        let saved = edit_rule(text, &scope, Some(TargetMode::ImeEnglish)).unwrap();
        let engine = RuleEngine::from_text(&saved).unwrap();
        assert_eq!(engine.len(), 1);
        assert_eq!(
            engine.match_target("app.exe", Some(ControlKind::Edit)),
            TargetMode::ImeEnglish
        );
        assert!(saved.contains("# untouched"));
        assert!(RuleEngine::from_text("[defaults]\nedti='ime-english'").is_err());
        assert!(RuleEngine::from_text("[defaults]\nedit='english'").is_err());
        assert!(RuleEngine::from_text("[[rules]]\nprocess='app.exe'\ncontrol='edit'\nmode='ime-english'\nelement={automation_id='', class_name='', control_type=50004}").is_err());
    }

    #[test]
    fn save_publishes_valid_config_and_rejects_stale_panel() {
        let path =
            std::env::temp_dir().join(format!("imeflow-panel-save-{}.toml", std::process::id()));
        let initial = "# user comment\n[defaults]\nedit='native-chinese'\n";
        std::fs::write(&path, initial).unwrap();
        let store = RuleStore::load(&path).unwrap();
        let old = store.snapshot();
        let scope = RuleScope::Global(ControlKind::Edit);
        store
            .save(initial, &scope, Some(TargetMode::ImeEnglish))
            .unwrap();
        assert_eq!(
            old.global_mode(Some(ControlKind::Edit)),
            TargetMode::NativeChinese
        );
        assert_eq!(
            store.snapshot().global_mode(Some(ControlKind::Edit)),
            TargetMode::ImeEnglish
        );
        assert_eq!(
            std::fs::read_to_string(path.with_extension("toml.bak")).unwrap(),
            initial
        );
        let saved = store.read_text().unwrap();
        assert!(saved.contains("# user comment"));
        assert!(store
            .save(initial, &scope, Some(TargetMode::NativeChinese))
            .is_err());
        assert_eq!(store.read_text().unwrap(), saved);
        let invalid = "[defaults]\nedit='not-a-mode'\n";
        std::fs::write(&path, invalid).unwrap();
        assert!(store
            .save(invalid, &scope, Some(TargetMode::NativeChinese))
            .is_err());
        assert_eq!(
            store.snapshot().global_mode(Some(ControlKind::Edit)),
            TargetMode::ImeEnglish
        );
        assert_eq!(store.read_text().unwrap(), invalid);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(path.with_extension("toml.bak")).unwrap();
    }

    #[test]
    fn reload_replaces_only_valid_rules_and_keeps_callback_snapshot() {
        let path = std::env::temp_dir().join(format!("imeflow-reload-{}.toml", std::process::id()));
        std::fs::write(
            &path,
            "[[rules]]\nprocess='example.exe'\nmode='native-chinese'",
        )
        .unwrap();
        let store = RuleStore::load(&path).unwrap();
        let old = store.snapshot();
        std::fs::write(
            &path,
            "[[rules]]\nprocess='example.exe'\nmode='ime-english'",
        )
        .unwrap();
        assert_eq!(store.reload().unwrap(), 1);
        assert_eq!(
            store.snapshot().match_process("example.exe"),
            TargetMode::ImeEnglish
        );
        assert_eq!(old.match_process("example.exe"), TargetMode::NativeChinese);
        std::fs::write(
            &path,
            "[[rules]]\nprocess='example.exe'\ncontorl='edit'\nmode='bad'",
        )
        .unwrap();
        let error = store.reload().unwrap_err();
        assert!(error.contains("第 1 条规则") && error.contains("control"));
        assert_eq!(
            store.snapshot().match_process("example.exe"),
            TargetMode::ImeEnglish
        );
        std::fs::remove_file(&path).unwrap();
        assert!(store.reload().is_err());
        assert_eq!(
            store.snapshot().match_process("example.exe"),
            TargetMode::ImeEnglish
        );
    }

    #[test]
    fn strict_rules_reject_typos_empty_names_and_duplicate_keys() {
        let valid =
            "[[rules]]\nprocess = ' Cursor.EXE '\ncontrol = ' edit '\nmode = 'native-chinese'\n";
        let engine = RuleEngine::from_text(valid).unwrap();
        assert_eq!(
            engine.match_target("cursor.exe", Some(ControlKind::Edit)),
            TargetMode::NativeChinese
        );
        assert!(RuleEngine::from_text(&format!("{valid}{valid}")).is_err());
        assert!(RuleEngine::from_text(&valid.replace(" Cursor.EXE ", "   ")).is_err());
        assert!(RuleEngine::from_text(&valid.replace(" Cursor.EXE ", "C:/Cursor.exe")).is_err());
        assert!(RuleEngine::from_text(&valid.replace("control =", "contorl =")).is_err());
        assert!(RuleEngine::from_text(&valid.replace("' edit '", "'unknown-control'")).is_err());
    }

    #[test]
    fn bundled_globals_cover_known_and_unknown_applications() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("rules.toml");
        let engine = RuleEngine::load_from(&path).unwrap();
        for exe in [
            "Cursor.exe",
            "Code.exe",
            "ChatGPT.exe",
            "Codex.exe",
            "new-app.exe",
        ] {
            assert_eq!(
                engine.match_target(exe, Some(ControlKind::Terminal)),
                TargetMode::ImeEnglish
            );
            assert_eq!(
                engine.match_target(exe, Some(ControlKind::Password)),
                TargetMode::ImeEnglish
            );
            assert_eq!(
                engine.match_target(exe, Some(ControlKind::Edit)),
                TargetMode::NativeChinese
            );
        }
    }

    #[test]
    fn standalone_terminals_default_to_ime_english() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("rules.toml");
        let engine = RuleEngine::load_from(&path).unwrap();
        for exe in [
            "WindowsTerminal.exe",
            "cmd.exe",
            "powershell.exe",
            "pwsh.exe",
            "conhost.exe",
        ] {
            assert_eq!(
                engine.match_target(exe, Some(ControlKind::Terminal)),
                TargetMode::ImeEnglish
            );
            assert_eq!(
                engine.match_target(exe, Some(ControlKind::Other)),
                TargetMode::ImeEnglish
            );
            assert_eq!(
                engine.match_target(exe, Some(ControlKind::Password)),
                TargetMode::ImeEnglish
            );
        }
        assert_eq!(
            engine.match_target("unlisted.exe", Some(ControlKind::Terminal)),
            TargetMode::ImeEnglish
        );
    }

    #[test]
    fn browsers_use_english_address_bar_and_chinese_page_inputs() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("rules.toml");
        let engine = RuleEngine::load_from(&path).unwrap();
        for exe in ["msedge.exe", "chrome.exe", "firefox.exe"] {
            assert_eq!(
                engine.match_target(exe, Some(ControlKind::AddressBar)),
                TargetMode::ImeEnglish
            );
            assert_eq!(
                engine.match_target(exe, Some(ControlKind::Terminal)),
                TargetMode::ImeEnglish
            );
            assert_eq!(
                engine.match_target(exe, Some(ControlKind::Edit)),
                TargetMode::NativeChinese
            );
            assert_eq!(
                engine.match_target(exe, Some(ControlKind::Other)),
                TargetMode::Passthrough
            );
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
