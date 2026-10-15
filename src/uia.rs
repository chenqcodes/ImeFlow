//! Inspect the focused UI Automation element (control-level IME rules).

use std::cell::RefCell;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, UIA_DocumentControlTypeId,
    UIA_EditControlTypeId, UIA_CONTROLTYPE_ID,
};
use windows::Win32::UI::Accessibility::{
    IUIAutomationCacheRequest, TreeScope_Element, UIA_AriaRolePropertyId, UIA_BoundingRectanglePropertyId,
    UIA_AutomationIdPropertyId, UIA_ClassNamePropertyId, UIA_ControlTypePropertyId,
    UIA_IsPasswordPropertyId, UIA_LocalizedControlTypePropertyId, UIA_NamePropertyId,
    UIA_NativeWindowHandlePropertyId, UIA_ProcessIdPropertyId,
};

thread_local! {
    static AUTOMATION: RefCell<Option<(IUIAutomation, IUIAutomationCacheRequest)>> = const { RefCell::new(None) };
}

fn automation() -> Option<(IUIAutomation, IUIAutomationCacheRequest)> {
    // COM apartments and interface ownership are per thread.
    AUTOMATION.with(|slot| {
        if slot.borrow().is_none() {
            unsafe {
                CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().ok()?;
            }
            let auto: IUIAutomation =
                unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()? };
            let cache = unsafe { auto.CreateCacheRequest().ok()? };
            unsafe {
                cache.SetTreeScope(TreeScope_Element).ok()?;
                for property in [
                    UIA_NamePropertyId,
                    UIA_ClassNamePropertyId,
                    UIA_AutomationIdPropertyId,
                    UIA_LocalizedControlTypePropertyId,
                    UIA_AriaRolePropertyId,
                    UIA_ControlTypePropertyId,
                    UIA_IsPasswordPropertyId,
                    UIA_ProcessIdPropertyId,
                    UIA_BoundingRectanglePropertyId,
                    UIA_NativeWindowHandlePropertyId,
                ] {
                    cache.AddProperty(property).ok()?;
                }
            }
            *slot.borrow_mut() = Some((auto, cache));
        }
        slot.borrow().clone()
    })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ControlKind {
    Terminal,
    AddressBar,
    Edit,
    Password,
    CodeEditor,
    #[default]
    Other,
}

impl ControlKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::AddressBar => "address-bar",
            Self::Edit => "edit",
            Self::Password => "password",
            Self::CodeEditor => "code-editor",
            Self::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "terminal" | "console" | "xterm" => Some(Self::Terminal),
            "address-bar" | "addressbar" | "omnibox" | "url" => Some(Self::AddressBar),
            "edit" | "input" | "textbox" | "chat" => Some(Self::Edit),
            "password" => Some(Self::Password),
            "code-editor" => Some(Self::CodeEditor),
            "other" | "*" => Some(Self::Other),
            _ => None,
        }
    }

    pub const ALL: [Self; 6] = [
        Self::Edit,
        Self::Terminal,
        Self::AddressBar,
        Self::Password,
        Self::CodeEditor,
        Self::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Edit => "普通输入框",
            Self::Terminal => "终端",
            Self::AddressBar => "浏览器地址栏",
            Self::Password => "密码框",
            Self::CodeEditor => "代码编辑区",
            Self::Other => "其他 / 无法识别的区域",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct FocusInfo {
    /// Screen rectangle for the opt-in picker overlay, never a persisted selector.
    pub bounding_rect: Option<[i32; 4]>,
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

impl FocusInfo {
    /// Ordinary logs retain classification evidence, never UIA names or ancestor text.
    pub fn log_summary(&self) -> String {
        let structure = format!(
            "{} {} {}",
            self.class_name, self.automation_id, self.breadcrumb
        )
        .to_ascii_lowercase();
        format!(
            "kind={} type={} navigation={} xterm={} monaco={} composer={} ancestors={}",
            self.kind.as_str(),
            self.control_type,
            self.is_navigation_control(),
            structure.contains("xterm"),
            structure.contains("monaco"),
            contains_any(
                &[&structure],
                &["composer", "ui-prompt-input", "chat-input"]
            ),
            self.breadcrumb
                .split(" < ")
                .filter(|part| !part.is_empty())
                .count()
        )
    }

    pub fn is_navigation_control(&self) -> bool {
        is_navigation_control(self.control_type, &self.aria_role)
    }

    /// Stronger evidence that an Edit element is a real chat/prompt input rather than
    /// xterm.js briefly moving focus through a helper textarea.
    pub fn looks_explicit_chat_input(&self) -> bool {
        let id = self.automation_id.to_ascii_lowercase();
        let path = self.breadcrumb.to_ascii_lowercase();
        if contains_any(
            &[&self.class_name.to_ascii_lowercase(), &path],
            &["monaco-editor", "overflow-guard"],
        ) && !contains_any(
            &[&id, &path],
            &["ui-prompt-input", "composer", "chat-input", "prompt-input"],
        ) {
            return false;
        }
        contains_any(
            &[&self.name.to_ascii_lowercase(), &id, &path],
            &[
                "codex",
                "chat",
                "ask",
                "prompt",
                "composer",
                "follow-up",
                "follow up",
                "question",
                "提问",
                "对话",
                "随心输入",
                "ui-prompt-input",
                "composer-messages",
            ],
        )
    }
}

fn bstr_to_string(b: windows::core::BSTR) -> String {
    b.to_string()
}

/// Snapshot the currently focused UIA element (+ parent breadcrumb).
pub fn inspect_focused() -> Option<FocusInfo> {
    let (auto, cache) = automation()?;
    let el = unsafe { auto.GetFocusedElementBuildCache(&cache).ok()? };
    Some(inspect_element(&auto, &cache, &el))
}

/// Opt-in picker check, never run on the automatic switching hot path.
/// The scope is an executable's matching identifiers, not a permanent RuntimeId.
pub fn identifier_unique_in_window(
    root: windows::Win32::Foundation::HWND,
    focus: &FocusInfo,
) -> bool {
    use windows::Win32::UI::Accessibility::TreeScope_Subtree;
    let check = || -> Option<bool> {
        if focus.automation_id.trim().is_empty() {
            return Some(false);
        }
        let (auto, cache) = automation()?;
        unsafe {
            let root = auto.ElementFromHandleBuildCache(root, &cache).ok()?;
            let condition = auto
                .CreatePropertyCondition(
                    UIA_AutomationIdPropertyId,
                    &windows::core::VARIANT::from(focus.automation_id.as_str()),
                )
                .ok()?;
            let matches = root
                .FindAllBuildCache(TreeScope_Subtree, &condition, &cache)
                .ok()?;
            // A shared automation ID is not specific enough, even if other fields differ.
            if matches.Length().ok()? != 1 {
                return Some(false);
            }
            let matched = matches.GetElement(0).ok()?;
            Some(element_runtime_id(&matched) == focus.runtime_id && !focus.runtime_id.is_empty())
        }
    };
    check().unwrap_or(false)
}

fn inspect_element(
    auto: &IUIAutomation,
    cache: &IUIAutomationCacheRequest,
    el: &IUIAutomationElement,
) -> FocusInfo {
    let name = unsafe { el.CachedName().ok().map(bstr_to_string).unwrap_or_default() };
    let class_name = unsafe {
        el.CachedClassName()
            .ok()
            .map(bstr_to_string)
            .unwrap_or_default()
    };
    let automation_id = unsafe {
        el.CachedAutomationId()
            .ok()
            .map(bstr_to_string)
            .unwrap_or_default()
    };
    let localized_type = unsafe {
        el.CachedLocalizedControlType()
            .ok()
            .map(bstr_to_string)
            .unwrap_or_default()
    };
    let aria_role = unsafe {
        el.CachedAriaRole()
            .ok()
            .map(bstr_to_string)
            .unwrap_or_default()
    };
    let control_type = unsafe {
        el.CachedControlType()
            .ok()
            .map(|c: UIA_CONTROLTYPE_ID| c.0)
            .unwrap_or(0)
    };
    let is_password = unsafe {
        el.CachedIsPassword()
            .ok()
            .map(|b| b.as_bool())
            .unwrap_or(false)
    };

    let (breadcrumb, owner_root) = ancestor_metadata(auto, cache, el);
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
        bounding_rect: unsafe { el.CachedBoundingRectangle().ok().map(|r| [r.left, r.top, r.right, r.bottom]) },
        name,
        class_name,
        automation_id,
        localized_type,
        aria_role,
        control_type,
        is_password,
        kind,
        breadcrumb,
        process_id: unsafe { el.CachedProcessId().unwrap_or(0).max(0) as u32 },
        owner_root,
        runtime_id: element_runtime_id(el),
    }
}

fn element_runtime_id(el: &IUIAutomationElement) -> Vec<i32> {
    use windows::Win32::System::Ole::{
        SafeArrayDestroy, SafeArrayGetDim, SafeArrayGetElement, SafeArrayGetElemsize,
        SafeArrayGetLBound, SafeArrayGetUBound,
    };
    unsafe {
        let Ok(array) = el.GetRuntimeId() else {
            return Vec::new();
        };
        if array.is_null() {
            return Vec::new();
        }
        let read = || -> Option<Vec<i32>> {
            if SafeArrayGetDim(array) != 1 || SafeArrayGetElemsize(array) != 4 {
                return None;
            }
            let lo = SafeArrayGetLBound(array, 1).ok()?;
            let hi = SafeArrayGetUBound(array, 1).ok()?;
            let count = i64::from(hi) - i64::from(lo) + 1;
            if !(1..=64).contains(&count) {
                return None;
            }
            let mut out = Vec::with_capacity(count as usize);
            for index in lo..=hi {
                let mut value = 0i32;
                SafeArrayGetElement(
                    array,
                    &index,
                    &mut value as *mut i32 as *mut std::ffi::c_void,
                )
                .ok()?;
                out.push(value);
            }
            Some(out)
        };
        let result = read().unwrap_or_default();
        let _ = SafeArrayDestroy(array);
        result
    }
}

fn ancestor_metadata(
    auto: &IUIAutomation,
    cache: &IUIAutomationCacheRequest,
    el: &IUIAutomationElement,
) -> (String, isize) {
    use windows::Win32::UI::Accessibility::UIA_WindowControlTypeId;
    let Ok(walker) = (unsafe { auto.RawViewWalker() }) else {
        return (String::new(), 0);
    };
    let mut current = el.clone();
    let mut parts = Vec::new();
    let mut owner_root = 0;
    let mut include_names = true;
    for depth in 0..20 {
        let hwnd = unsafe { current.CachedNativeWindowHandle().unwrap_or_default() };
        if owner_root == 0 && !hwnd.0.is_null() {
            owner_root = crate::process::root_hwnd(hwnd).0 as isize;
        }
        if unsafe { current.CachedControlType().ok() } == Some(UIA_WindowControlTypeId) {
            include_names = false; // Window titles must never classify an input region.
        }
        if depth > 0 && depth <= 8 && include_names {
            let n = unsafe {
                current
                    .CachedName()
                    .ok()
                    .map(bstr_to_string)
                    .unwrap_or_default()
            };
            let t = unsafe {
                current
                    .CachedLocalizedControlType()
                    .ok()
                    .map(bstr_to_string)
                    .unwrap_or_default()
            };
            let id = unsafe {
                current
                    .CachedAutomationId()
                    .ok()
                    .map(bstr_to_string)
                    .unwrap_or_default()
            };
            let class = unsafe {
                current
                    .CachedClassName()
                    .ok()
                    .map(bstr_to_string)
                    .unwrap_or_default()
            };
            if !n.is_empty() || !t.is_empty() || !id.is_empty() || !class.is_empty() {
                parts.push(format!("{t}:{n}#{id}[{class}]"));
            }
        }
        if owner_root != 0 && (!include_names || depth >= 8) {
            break;
        }
        let Ok(parent) = (unsafe { walker.GetParentElementBuildCache(&current, cache) }) else {
            break;
        };
        current = parent;
    }
    (parts.join(" < "), owner_root)
}
/// Pure classifier — unit-tested without COM.
#[allow(clippy::too_many_arguments)]
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
    // Navigation labels and ancestor text can mention a terminal without being its input.
    if is_navigation_control(control_type, aria_role) {
        return ControlKind::Other;
    }

    let name_l = name.to_ascii_lowercase();
    let class_l = class_name.to_ascii_lowercase();
    let id_l = automation_id.to_ascii_lowercase();
    let loc_l = localized_type.to_ascii_lowercase();
    let role_l = aria_role.to_ascii_lowercase();
    let path_l = breadcrumb.to_ascii_lowercase();

    // A filename or code text is not evidence of a chat/terminal control.
    let monaco_code = contains_any(
        &[&class_l, &path_l],
        &["monaco-editor", "monaco-mouse-cursor-text"],
    ) || (path_l.contains("overflow-guard") && path_l.contains("monaco"));
    let structured_chat = contains_any(
        &[&id_l, &class_l, &path_l],
        &["ui-prompt-input", "composer", "chat-input", "prompt-input"],
    );
    if monaco_code && !structured_chat {
        return ControlKind::CodeEditor;
    }
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
            return ControlKind::CodeEditor;
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

fn is_navigation_control(control_type: i32, aria_role: &str) -> bool {
    use windows::Win32::UI::Accessibility::{
        UIA_ButtonControlTypeId, UIA_HyperlinkControlTypeId, UIA_ListItemControlTypeId,
        UIA_MenuItemControlTypeId, UIA_TabItemControlTypeId, UIA_TreeItemControlTypeId,
    };
    let role = aria_role.to_ascii_lowercase();
    // Some terminal providers expose their editable helper with a Button UIA type.
    if role
        .split_whitespace()
        .any(|r| matches!(r, "textbox" | "searchbox"))
    {
        return false;
    }
    role.split_whitespace().any(|r| {
        matches!(
            r,
            "button" | "link" | "tab" | "menuitem" | "treeitem" | "listitem"
        )
    }) || [
        UIA_ButtonControlTypeId,
        UIA_HyperlinkControlTypeId,
        UIA_ListItemControlTypeId,
        UIA_MenuItemControlTypeId,
        UIA_TabItemControlTypeId,
        UIA_TreeItemControlTypeId,
    ]
    .iter()
    .any(|kind| kind.0 == control_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_log_summary_excludes_input_and_ancestor_text() {
        let info = FocusInfo {
            name: "private message".into(),
            automation_id: "private-id".into(),
            breadcrumb: "group:private conversation#[composer]".into(),
            ..Default::default()
        };
        let summary = info.log_summary();
        assert!(!summary.contains("private"));
        assert!(summary.contains("composer=true"));
    }

    #[test]
    fn navigation_labels_and_ancestor_content_cannot_be_terminal_inputs() {
        for name in ["终端", "Terminal", "Review project"] {
            let info = FocusInfo {
                control_type: 50000,
                aria_role: "button".into(),
                ..Default::default()
            };
            assert!(info.is_navigation_control());
            assert_eq!(
                classify_focus(
                    50000,
                    false,
                    name,
                    "",
                    "",
                    "button",
                    "button",
                    "组:#[contents] < 列表项目:聊天内容提到 terminal 和 xterm"
                ),
                ControlKind::Other
            );
        }
        // A genuine input retains terminal classification, including unusual providers.
        assert!(!is_navigation_control(50000, "textbox"));
        assert_eq!(
            classify_focus(
                50004,
                false,
                "Terminal input",
                "",
                "",
                "edit",
                "textbox",
                "组:#[xterm-helpers] < 组:#[xterm-screen]"
            ),
            ControlKind::Terminal
        );
    }

    #[test]
    fn code_names_do_not_override_monaco_structure() {
        for name in [
            "agent.rs",
            "chat.rs",
            "task.rs",
            "terminal.rs",
            "address bar.rs",
        ] {
            assert_eq!(
                classify_focus(
                    UIA_EditControlTypeId.0,
                    false,
                    name,
                    "",
                    "",
                    "edit",
                    "textbox",
                    "group:#[monaco-editor] < group:#[overflow-guard]"
                ),
                ControlKind::CodeEditor
            );
            assert!(!FocusInfo {
                name: name.into(),
                breadcrumb: "group:#[monaco-editor]".into(),
                ..Default::default()
            }
            .looks_explicit_chat_input());
        }
    }

    #[test]
    fn structured_composer_can_use_a_monaco_editor() {
        assert_eq!(
            classify_focus(
                UIA_EditControlTypeId.0,
                false,
                "",
                "",
                "",
                "edit",
                "textbox",
                "group:#[monaco-editor] < group:#[ui-prompt-input] < group:#[composer]"
            ),
            ControlKind::Edit
        );
    }

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
    fn monaco_code_edit_is_code_not_chat() {
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
        assert_eq!(k, ControlKind::CodeEditor);
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
