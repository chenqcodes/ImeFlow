//! Inspect the focused UI Automation element (control-level IME rules).

use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, UIA_DocumentControlTypeId,
    UIA_EditControlTypeId, UIA_CONTROLTYPE_ID,
};

static COM: std::sync::Once = std::sync::Once::new();

fn ensure_com() {
    COM.call_once(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    });
}

fn automation() -> Option<IUIAutomation> {
    ensure_com();
    // ponytail: recreate per call — IUIAutomation is !Send; V0 stays on message thread.
    unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok() }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    Terminal,
    AddressBar,
    Edit,
    Password,
    Other,
}

impl ControlKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::AddressBar => "address-bar",
            Self::Edit => "edit",
            Self::Password => "password",
            Self::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "terminal" | "console" | "xterm" => Some(Self::Terminal),
            "address-bar" | "addressbar" | "omnibox" | "url" => Some(Self::AddressBar),
            "edit" | "input" | "textbox" | "chat" => Some(Self::Edit),
            "password" => Some(Self::Password),
            "other" | "*" => Some(Self::Other),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct FocusInfo {
    pub name: String,
    pub class_name: String,
    pub automation_id: String,
    pub localized_type: String,
    pub aria_role: String,
    pub control_type: i32,
    pub is_password: bool,
    pub kind: ControlKind,
    pub breadcrumb: String,
    pub process_id: u32,
    pub owner_root: isize,
    /// Opaque UIA RuntimeId, never an inferred identity from text contents.
    pub runtime_id: Vec<i32>,
}

impl Default for ControlKind {
    fn default() -> Self {
        Self::Other
    }
}

impl FocusInfo {
    pub fn summarize(&self) -> String {
        format!(
            "{} | type={} name=\"{}\" role=\"{}\" id=\"{}\" path=\"{}\"",
            self.kind.as_str(),
            self.control_type,
            trunc(&self.name, 36),
            trunc(&self.aria_role, 20),
            trunc(&self.automation_id, 20),
            trunc(&self.breadcrumb, 48)
        )
    }

    /// Stronger evidence that an Edit element is a real chat/prompt input rather than
    /// xterm.js briefly moving focus through a helper textarea.
    pub fn looks_explicit_chat_input(&self) -> bool {
        let name = self.name.to_ascii_lowercase();
        let id = self.automation_id.to_ascii_lowercase();
        let path = self.breadcrumb.to_ascii_lowercase();
        contains_any(
            &[&name, &id, &path],
            &[
                "codex", "chat", "ask", "prompt", "composer", "follow-up",
                "follow up", "question", "提问", "对话", "随心输入", "ui-prompt-input",
                "composer-messages",
            ],
        )
    }
}

fn trunc(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        format!("{t}…")
    } else {
        t
    }
}

fn bstr_to_string(b: windows::core::BSTR) -> String {
    b.to_string()
}

/// Snapshot the currently focused UIA element (+ parent breadcrumb).
pub fn inspect_focused() -> Option<FocusInfo> {
    let auto = automation()?;
    let el = unsafe { auto.GetFocusedElement().ok()? };
    Some(inspect_element(&auto, &el))
}

fn inspect_element(auto: &IUIAutomation, el: &IUIAutomationElement) -> FocusInfo {
    let name = unsafe { el.CurrentName().ok().map(bstr_to_string).unwrap_or_default() };
    let class_name = unsafe {
        el.CurrentClassName()
            .ok()
            .map(bstr_to_string)
            .unwrap_or_default()
    };
    let automation_id = unsafe {
        el.CurrentAutomationId()
            .ok()
            .map(bstr_to_string)
            .unwrap_or_default()
    };
    let localized_type = unsafe {
        el.CurrentLocalizedControlType()
            .ok()
            .map(bstr_to_string)
            .unwrap_or_default()
    };
    let aria_role = unsafe {
        el.CurrentAriaRole()
            .ok()
            .map(bstr_to_string)
            .unwrap_or_default()
    };
    let control_type = unsafe {
        el.CurrentControlType()
            .ok()
            .map(|c: UIA_CONTROLTYPE_ID| c.0)
            .unwrap_or(0)
    };
    let is_password = unsafe {
        el.CurrentIsPassword()
            .ok()
            .map(|b| b.as_bool())
            .unwrap_or(false)
    };

    let breadcrumb = parent_breadcrumb(auto, el, 8);
    let kind = classify_focus(
        control_type,
        is_password,
        &name,
        &class_name,
        &automation_id,
        &localized_type,
        &aria_role,
        &breadcrumb,
    );

    FocusInfo {
        name,
        class_name,
        automation_id,
        localized_type,
        aria_role,
        control_type,
        is_password,
        kind,
        breadcrumb,
        process_id: unsafe { el.CurrentProcessId().unwrap_or(0).max(0) as u32 },
        owner_root: element_owner_root(auto, el),
        runtime_id: element_runtime_id(el),
    }
}

fn element_runtime_id(el: &IUIAutomationElement) -> Vec<i32> {
    use windows::Win32::System::Ole::{
        SafeArrayDestroy, SafeArrayGetDim, SafeArrayGetElement, SafeArrayGetElemsize,
        SafeArrayGetLBound, SafeArrayGetUBound,
    };
    unsafe {
        let Ok(array) = el.GetRuntimeId() else { return Vec::new() };
        if array.is_null() { return Vec::new(); }
        let read = || -> Option<Vec<i32>> {
            if SafeArrayGetDim(array) != 1 || SafeArrayGetElemsize(array) != 4 { return None; }
            let lo = SafeArrayGetLBound(array, 1).ok()?;
            let hi = SafeArrayGetUBound(array, 1).ok()?;
            let count = i64::from(hi) - i64::from(lo) + 1;
            if !(1..=64).contains(&count) { return None; }
            let mut out = Vec::with_capacity(count as usize);
            for index in lo..=hi {
                let mut value = 0i32;
                SafeArrayGetElement(array, &index,
                    &mut value as *mut i32 as *mut std::ffi::c_void).ok()?;
                out.push(value);
            }
            Some(out)
        };
        let result = read().unwrap_or_default();
        let _ = SafeArrayDestroy(array);
        result
    }
}

fn element_owner_root(auto: &IUIAutomation, el: &IUIAutomationElement) -> isize {
    let Ok(walker) = (unsafe { auto.RawViewWalker() }) else { return 0 };
    let mut current = el.clone();
    for _ in 0..20 {
        let hwnd = unsafe { current.CurrentNativeWindowHandle().unwrap_or_default() };
        if !hwnd.0.is_null() {
            return crate::process::root_hwnd(hwnd).0 as isize;
        }
        let Ok(parent) = (unsafe { walker.GetParentElement(&current) }) else { break };
        current = parent;
    }
    0
}

fn parent_breadcrumb(auto: &IUIAutomation, el: &IUIAutomationElement, depth: usize) -> String {
    use windows::Win32::UI::Accessibility::UIA_WindowControlTypeId;
    let mut parts = Vec::new();
    // Raw view retains xterm/helper ancestors that control view may omit.
    let Ok(walker) = (unsafe { auto.RawViewWalker() }) else { return String::new() };
    let mut cur = el.clone();
    for _ in 0..depth {
        let Ok(p) = (unsafe { walker.GetParentElement(&cur) }) else { break };
        if unsafe { p.CurrentControlType().ok() } == Some(UIA_WindowControlTypeId) {
            break; // A window title is not evidence that its focused child is a chat box.
        }
        let n = unsafe { p.CurrentName().ok().map(bstr_to_string).unwrap_or_default() };
        let t = unsafe { p.CurrentLocalizedControlType().ok().map(bstr_to_string).unwrap_or_default() };
        let id = unsafe { p.CurrentAutomationId().ok().map(bstr_to_string).unwrap_or_default() };
        let class = unsafe { p.CurrentClassName().ok().map(bstr_to_string).unwrap_or_default() };
        if !n.is_empty() || !t.is_empty() || !id.is_empty() || !class.is_empty() {
            parts.push(format!("{t}:{n}#{id}[{class}]"));
        }
        cur = p;
    }
    parts.join(" < ")
}

/// Pure classifier — unit-tested without COM.
pub fn classify_focus(
    control_type: i32,
    is_password: bool,
    name: &str,
    class_name: &str,
    automation_id: &str,
    localized_type: &str,
    aria_role: &str,
    breadcrumb: &str,
) -> ControlKind {
    if is_password {
        return ControlKind::Password;
    }

    let name_l = name.to_ascii_lowercase();
    let class_l = class_name.to_ascii_lowercase();
    let id_l = automation_id.to_ascii_lowercase();
    let loc_l = localized_type.to_ascii_lowercase();
    let role_l = aria_role.to_ascii_lowercase();
    let path_l = breadcrumb.to_ascii_lowercase();

    // Browser chrome: detect the omnibox/address bar before treating every Edit as a page input.
    // Keep the signals deliberately specific so ordinary web search boxes remain Edit → 中文.
    let address_bar = contains_any(
        &[&name_l, &class_l, &id_l, &path_l],
        &[
            "address and search bar",
            "address bar",
            "search or enter web address",
            "search with google or enter address",
            "or enter address",
            "or enter web address",
            "omnibox",
            "locationbar",
            "location bar",
            "地址和搜索栏",
            "地址栏和搜索栏",
            "搜索或输入网址",
            "搜索或输入 web 地址",
        ],
    );
    if address_bar {
        return ControlKind::AddressBar;
    }

    // Ancestor / self terminal signals win over chat keywords (Codex panel may sit nearby),
    // but chat message surfaces must not become Terminal just because a weak keyword matched.
    let strong_xterm = contains_any(
        &[&name_l, &class_l, &id_l, &path_l],
        &[
            "xterm",
            "xterm-helper",
            "xterm-screen",
            "xterm-rows",
            "terminal.integrated",
            "workbench.panel.terminal",
            "termcontrol",
            "pty.host",
            "webssh",
            "ttyd",
        ],
    );
    let chat_message_surface = contains_any(
        &[&name_l, &id_l, &path_l],
        &[
            "composer-messages",
            "composer-message",
            "chat-messages",
            "chat-message",
            "messages-container",
            "markdown-preview",
        ],
    );
    let terminal = contains_any(
        &[&name_l, &class_l, &id_l, &loc_l, &path_l],
        &[
            "terminal",
            "termcontrol",
            "xterm",
            "xterm-screen",
            "xterm-rows",
            "xterm-helpers",
            "console",
            "powershell",
            "cmd.exe",
            "shell integration",
            "pty.host",
            "terminal.integrated",
            "workbench.panel.terminal",
            "xterm-helper",
            "webssh",
            "ttyd",
            "ssh-terminal",
            "ssh终端",
            "终端",
            "控制台",
        ],
    );
    if terminal && !(chat_message_surface && !strong_xterm) {
        return ControlKind::Terminal;
    }

    // Chat / Codex / prompt boxes — prefer name/id/role/path, not WinRT class "*Input*".
    let chat_edit = contains_any(
        &[&name_l, &id_l, &role_l, &path_l, &loc_l],
        &[
            "codex",
            "chat",
            "ask",
            "prompt",
            "composer",
            "comment",
            "follow-up",
            "follow up",
            "agent",
            "question",
            "提问",
            "对话",
            "随心输入",
            "ui-prompt-input",
        ],
    );

    let textbox_role = role_l.contains("textbox")
        || role_l.contains("searchbox")
        || loc_l.contains("edit")
        || loc_l.contains("编辑")
        || loc_l.contains("文本");

    // Monaco 代码区在 Cursor 里经常是 Edit(50004)，不能当聊天框切中。
    let monaco_code = contains_any(
        &[&name_l, &id_l, &class_l, &path_l],
        &["monaco-editor", "monaco-mouse-cursor-text"],
    ) || (path_l.contains("overflow-guard") && path_l.contains("monaco"));

    if control_type == UIA_EditControlTypeId.0 {
        if monaco_code && !chat_edit {
            return ControlKind::Other;
        }
        return ControlKind::Edit;
    }

    // Electron contenteditable / Monaco-like docs used as chat input.
    if control_type == UIA_DocumentControlTypeId.0 {
        if chat_edit || textbox_role {
            return ControlKind::Edit;
        }
        // Plain code editor document → other (process default EN).
        return ControlKind::Other;
    }

    if chat_edit || textbox_role {
        return ControlKind::Edit;
    }

    ControlKind::Other
}

fn contains_any(haystacks: &[&str], needles: &[&str]) -> bool {
    needles
        .iter()
        .any(|n| haystacks.iter().any(|h| h.contains(n)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_wins_even_if_codex_nearby() {
        let k = classify_focus(
            UIA_DocumentControlTypeId.0,
            false,
            "",
            "",
            "",
            "document",
            "",
            "pane:Terminal 1#terminal < pane:Codex",
        );
        assert_eq!(k, ControlKind::Terminal);
    }

    #[test]
    fn codex_prompt_document_is_edit() {
        let k = classify_focus(
            UIA_DocumentControlTypeId.0,
            false,
            "",
            "",
            "",
            "document",
            "textbox",
            "pane:Codex < pane:Agent",
        );
        assert_eq!(k, ControlKind::Edit);
    }

    #[test]
    fn monaco_code_document_is_other() {
        let k = classify_focus(
            UIA_DocumentControlTypeId.0,
            false,
            "main.rs",
            "",
            "",
            "document",
            "",
            "pane:main.rs < pane:Editor",
        );
        assert_eq!(k, ControlKind::Other);
    }

    #[test]
    fn monaco_code_edit_is_other_not_chat() {
        // 真实 Cursor Monaco 常是 type=50004 Edit，不是 Document。
        let k = classify_focus(
            UIA_EditControlTypeId.0,
            false,
            "现在无法访问编辑器。 若要启用屏幕阅读器优化模式，请使用 Shift+A…",
            "",
            "",
            "",
            "textbox",
            "组:#[overflow-guard] < 代码:#[monaco-editor modified] < 组:#[editor-instance]",
        );
        assert_eq!(k, ControlKind::Other);
    }

    #[test]
    fn monaco_nearby_chat_prompt_stays_edit() {
        let k = classify_focus(
            UIA_EditControlTypeId.0,
            false,
            "",
            "",
            "",
            "",
            "textbox",
            "组:#[ui-prompt-input-editor__content] < 组:#[composer]",
        );
        assert_eq!(k, ControlKind::Edit);
    }

    #[test]
    fn xterm_class_is_terminal() {
        let k = classify_focus(
            50000,
            false,
            "",
            "xterm-helper-textarea",
            "",
            "",
            "textbox",
            "pane:Terminal",
        );
        assert_eq!(k, ControlKind::Terminal);
    }

    #[test]
    fn web_panel_xterm_is_terminal() {
        // 宝塔等网页 SSH：DOM 常是 xterm-screen / webssh，易被当成普通 Edit→中。
        let k = classify_focus(
            UIA_EditControlTypeId.0,
            false,
            "",
            "",
            "xterm-screen",
            "",
            "textbox",
            "组:webssh < 组:xterm",
        );
        assert_eq!(k, ControlKind::Terminal);
    }

    #[test]
    fn winrt_input_class_alone_is_not_edit() {
        let k = classify_focus(
            50033,
            false,
            "",
            "Windows.UI.Input.InputSite.WindowClass",
            "",
            "窗格",
            "",
            "窗口:",
        );
        assert_eq!(k, ControlKind::Other);
    }

    #[test]
    fn composer_messages_are_not_terminal() {
        let k = classify_focus(
            50026,
            false,
            "你是没有改东西吗？我刚试了试好像可以了",
            "",
            "",
            "组",
            "group",
            "组:#[] < 组:#[composer-messages-container] < 组:#[]",
        );
        assert_ne!(k, ControlKind::Terminal);
    }

    #[test]
    fn real_xterm_still_wins_near_composer() {
        let k = classify_focus(
            50004,
            false,
            "Terminal input",
            "",
            "",
            "",
            "textbox",
            "组:#[xterm-helpers] < 组:#[xterm-screen] < 组:#[composer]",
        );
        assert_eq!(k, ControlKind::Terminal);
    }

    #[test]
    fn browser_address_bar_is_not_plain_edit() {
        let k = classify_focus(
            UIA_EditControlTypeId.0,
            false,
            "Address and search bar",
            "OmniboxViewViews",
            "",
            "edit",
            "textbox",
            "工具栏:#[] < 组:#[location-bar]",
        );
        assert_eq!(k, ControlKind::AddressBar);
    }

    #[test]
    fn browser_page_textbox_stays_edit() {
        let k = classify_focus(
            UIA_EditControlTypeId.0,
            false,
            "搜索内容",
            "",
            "",
            "edit",
            "textbox",
            "文档:网页内容#RootWebArea[]",
        );
        assert_eq!(k, ControlKind::Edit);
    }

    #[test]
    fn explicit_chat_input_is_distinguished_from_generic_edit() {
        let chat = FocusInfo {
            name: "Ask Codex".into(),
            ..Default::default()
        };
        assert!(chat.looks_explicit_chat_input());

        let helper = FocusInfo {
            aria_role: "textbox".into(),
            ..Default::default()
        };
        assert!(!helper.looks_explicit_chat_input());
    }
}
