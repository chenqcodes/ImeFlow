//! Native rule list and reviewed configuration changes. All writes use RuleStore.
use crate::{
    mode::TargetMode,
    rules::{apply_preset, RuleEngine, RuleEntry, RuleScope, RuleStore, PRESETS},
    uia::ControlKind,
};
use std::{cell::RefCell, collections::BTreeSet, path::PathBuf, sync::Arc};
use windows::{
    core::{w, PCWSTR, PWSTR},
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Gdi::{
            CreateFontW, DeleteObject, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
            DEFAULT_PITCH, HFONT, OUT_DEFAULT_PRECIS,
        },
        UI::{Controls::Dialogs::*, HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
    },
};

const FILTER: usize = 301;
const LIST: usize = 302;
const DETAIL: usize = 303;
const MODE: usize = 304;
const SAVE: usize = 305;
const DELETE: usize = 306;
const REFRESH: usize = 307;
const PICK: usize = 308;
const IMPORT: usize = 309;
const EXPORT: usize = 310;
const RESTORE: usize = 311;
const APPLY: usize = 312;
const PRESET: usize = 313;
const PREVIEW_PRESET: usize = 314;
const STATUS: usize = 315;
const MODES: [TargetMode; 4] = [
    TargetMode::NativeChinese,
    TargetMode::ImeEnglish,
    TargetMode::Passthrough,
    TargetMode::LayoutEnglish,
];
struct Pending {
    text: String,
    expected: String,
}
struct Manager {
    hwnd: HWND,
    font: HFONT,
    _heading: crate::settings_ui::Heading,
    store: Arc<RuleStore>,
    original: String,
    entries: Vec<RuleEntry>,
    visible: Vec<usize>,
    filters: Vec<String>,
    pending: Option<Pending>,
}
thread_local! { static MANAGER: RefCell<Option<Manager>> = const { RefCell::new(None) }; }

pub fn open(store: Arc<RuleStore>) -> Result<(), String> {
    open_inner(store, true)
}
fn open_inner(store: Arc<RuleStore>, visible: bool) -> Result<(), String> {
    close();
    let original = store.read_text()?;
    let entries = RuleEngine::from_text(&original)?.entries();
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
            .map_err(|e| e.to_string())?;
        let class = w!("ImeFlowRuleManager");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: crate::settings_ui::background(),
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class,
            w!("ImeFlow · 规则管理"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            900,
            720,
            None,
            None,
            instance,
            None,
        )
        .map_err(|e| e.to_string())?;
        let mut monitor = windows::Win32::Graphics::Gdi::MONITORINFO {
            cbSize: std::mem::size_of::<windows::Win32::Graphics::Gdi::MONITORINFO>() as u32,
            ..Default::default()
        };
        let handle = windows::Win32::Graphics::Gdi::MonitorFromWindow(
            hwnd,
            windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST,
        );
        let dpi = if windows::Win32::Graphics::Gdi::GetMonitorInfoW(handle, &mut monitor).as_bool()
        {
            (GetDpiForWindow(hwnd).max(96) as i32)
                .min((monitor.rcWork.right - monitor.rcWork.left - 24) * 96 / 820)
                .min((monitor.rcWork.bottom - monitor.rcWork.top - 24) * 96 / 580)
                .max(48)
        } else {
            GetDpiForWindow(hwnd).max(96) as i32
        };
        let s = |v: i32| v * dpi / 96;
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            s(820),
            s(580),
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        let heading = crate::settings_ui::Heading::new(dpi);
        let font = CreateFontW(
            -s(14),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            DEFAULT_PITCH.0 as u32,
            w!("Microsoft YaHei UI"),
        );
        let make =
            |class: PCWSTR, title: &str, id, style, x, y, width, height| -> Result<(), String> {
                let style = if class.to_string().is_ok_and(|name| name == "BUTTON") {
                    style | WINDOW_STYLE(BS_OWNERDRAW as u32)
                } else {
                    style
                };
                let title = wide(title);
                let child = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    class,
                    PCWSTR(title.as_ptr()),
                    WS_CHILD | WS_VISIBLE | style,
                    s(x),
                    s(y),
                    s(width),
                    s(height),
                    hwnd,
                    HMENU(id as *mut _),
                    instance,
                    None,
                )
                .map_err(|e| e.to_string())?;
                SendMessageW(
                    child,
                    WM_SETFONT,
                    WPARAM(if id == 900 {
                        heading.0 .0 as usize
                    } else {
                        font.0 as usize
                    }),
                    LPARAM(1),
                );
                if class.to_string().is_ok_and(|name| name == "EDIT") {
                    let mut rect = windows::Win32::Foundation::RECT::default();
                    let _ = GetClientRect(child, &mut rect);
                    let pad = 12 * dpi / 96;
                    rect.left += pad;
                    rect.top += pad;
                    rect.right -= pad;
                    rect.bottom -= pad;
                    SendMessageW(
                        child,
                        windows::Win32::UI::Controls::EM_SETRECT,
                        WPARAM(0),
                        LPARAM(&rect as *const _ as isize),
                    );
                }
                Ok(())
            };
        let result = (|| {
            make(
                w!("STATIC"),
                "规则管理",
                900,
                WINDOW_STYLE(0),
                20,
                16,
                300,
                30,
            )?;
            make(w!("BUTTON"), "点选新增", PICK, WS_TABSTOP, 660, 16, 120, 32)?;
            make(
                w!("COMBOBOX"),
                "",
                FILTER,
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                20,
                66,
                302,
                240,
            )?;
            make(w!("BUTTON"), "刷新", REFRESH, WS_TABSTOP, 332, 62, 62, 30)?;
            make(
                w!("STATIC"),
                "规则详情 / 预览",
                0,
                WINDOW_STYLE(0),
                416,
                68,
                364,
                22,
            )?;
            make(
                w!("LISTBOX"),
                "",
                LIST,
                WINDOW_STYLE(
                    LBS_NOTIFY as u32
                        | LBS_NOINTEGRALHEIGHT as u32
                        | LBS_OWNERDRAWFIXED as u32
                        | LBS_HASSTRINGS as u32,
                ) | WS_VSCROLL
                    | WS_TABSTOP,
                20,
                104,
                374,
                260,
            )?;
            make(
                w!("EDIT"),
                "",
                DETAIL,
                WINDOW_STYLE(ES_MULTILINE as u32 | ES_READONLY as u32 | ES_AUTOVSCROLL as u32)
                    | WS_VSCROLL
                    | WS_TABSTOP,
                416,
                104,
                364,
                157,
            )?;
            make(
                w!("STATIC"),
                "输入模式",
                0,
                WINDOW_STYLE(0),
                416,
                276,
                364,
                22,
            )?;
            make(
                w!("COMBOBOX"),
                "",
                MODE,
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP,
                416,
                302,
                364,
                180,
            )?;
            make(w!("BUTTON"), "保存", SAVE, WS_TABSTOP, 416, 344, 108, 32)?;
            make(
                w!("BUTTON"),
                "删除覆盖 / 恢复默认",
                DELETE,
                WS_TABSTOP,
                536,
                344,
                244,
                32,
            )?;
            make(
                w!("STATIC"),
                "常用预设",
                0,
                WINDOW_STYLE(0),
                20,
                397,
                80,
                22,
            )?;
            make(
                w!("COMBOBOX"),
                "",
                PRESET,
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP,
                104,
                393,
                544,
                180,
            )?;
            make(
                w!("BUTTON"),
                "预览",
                PREVIEW_PRESET,
                WS_TABSTOP,
                660,
                389,
                120,
                32,
            )?;
            for (id, label, x, width) in [
                (IMPORT, "导入", 20, 94),
                (EXPORT, "导出", 124, 94),
                (RESTORE, "恢复上一份", 228, 132),
                (APPLY, "应用预览 · 替换配置", 416, 364),
            ] {
                make(w!("BUTTON"), label, id, WS_TABSTOP, x, 442, width, 32)?;
            }
            make(w!("STATIC"), "", STATUS, WINDOW_STYLE(0), 20, 494, 760, 48)?;
            Ok::<_, String>(())
        })();
        if let Err(e) = result {
            let _ = DestroyWindow(hwnd);
            let _ = DeleteObject(font);
            return Err(e);
        }
        send(hwnd, LIST, LB_SETITEMHEIGHT, 0, s(40) as isize);
        for mode in MODES {
            combo_add(hwnd, MODE, mode.description());
        }
        for preset in PRESETS {
            combo_add(hwnd, PRESET, preset);
        }
        send(hwnd, PRESET, CB_SETCURSEL, 0, 0);
        MANAGER.with(|slot| {
            *slot.borrow_mut() = Some(Manager {
                hwnd,
                font,
                _heading: heading,
                store,
                original,
                entries,
                visible: Vec::new(),
                filters: Vec::new(),
                pending: None,
            })
        });
        fill_filters();
        if visible {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
    }
    Ok(())
}
pub fn close() {
    let old = MANAGER.with(|slot| slot.borrow_mut().take());
    if let Some(old) = old {
        unsafe {
            let _ = DestroyWindow(old.hwnd);
            let _ = DeleteObject(old.font);
        }
    }
}
pub fn handle_message(message: &MSG) -> bool {
    let hwnd = MANAGER.with(|slot| slot.borrow().as_ref().map(|m| m.hwnd));
    let Some(hwnd) = hwnd else {
        return false;
    };
    unsafe {
        if message.hwnd != hwnd && !IsChild(hwnd, message.hwnd).as_bool() {
            return false;
        }
        if message.message == WM_KEYDOWN && message.wParam.0 == 0x1b {
            close();
            return true;
        }
        IsDialogMessageW(hwnd, message).as_bool()
    }
}
fn context() -> Option<(HWND, Arc<RuleStore>, String)> {
    MANAGER.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|m| (m.hwnd, Arc::clone(&m.store), m.original.clone()))
    })
}
fn fill_filters() {
    MANAGER.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(m) = slot.as_mut() else {
            return;
        };
        let old = m
            .filters
            .get(send(m.hwnd, FILTER, CB_GETCURSEL, 0, 0) as usize)
            .cloned()
            .unwrap_or("*".into());
        let processes: BTreeSet<_> = m
            .entries
            .iter()
            .filter_map(|e| e.scope.process().map(str::to_owned))
            .collect();
        m.filters = vec!["*".into(), String::new()];
        m.filters.extend(processes);
        send(m.hwnd, FILTER, CB_RESETCONTENT, 0, 0);
        for item in &m.filters {
            combo_add(
                m.hwnd,
                FILTER,
                if item == "*" {
                    "全部规则"
                } else if item.is_empty() {
                    "仅通用默认"
                } else {
                    item
                },
            );
        }
        send(
            m.hwnd,
            FILTER,
            CB_SETCURSEL,
            m.filters.iter().position(|f| f == &old).unwrap_or(0),
            0,
        );
    });
    fill_rows();
}
fn filter_entries(entries: &[RuleEntry], filter: &str) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            filter == "*"
                || match e.scope.process() {
                    Some(p) => p.eq_ignore_ascii_case(filter),
                    None => filter.is_empty(),
                }
        })
        .map(|(i, _)| i)
        .collect()
}
fn fill_rows() {
    MANAGER.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(m) = slot.as_mut() else {
            return;
        };
        m.pending = None;
        let filter = m
            .filters
            .get(send(m.hwnd, FILTER, CB_GETCURSEL, 0, 0) as usize)
            .map(String::as_str)
            .unwrap_or("*");
        m.visible = filter_entries(&m.entries, filter);
        send(m.hwnd, LIST, LB_RESETCONTENT, 0, 0);
        for &index in &m.visible {
            let e = &m.entries[index];
            let label = wide(&format!(
                "{} → {}{}",
                e.scope.label(),
                e.mode.description(),
                if e.configured {
                    ""
                } else {
                    "（未配置，兜底）"
                }
            ));
            send(m.hwnd, LIST, LB_ADDSTRING, 0, label.as_ptr() as isize);
        }

        send(m.hwnd, LIST, LB_SETCURSEL, 0, 0);
        enable(m.hwnd, APPLY, false);
        set_text(
            m.hwnd,
            STATUS,
            &format!(
                "{} 项显示。配置：{}\r\n修改后请重新进入目标输入区域；外部修改后先刷新。",
                m.visible.len(),
                m.store.path().display()
            ),
        );
    });
    show_selected();
}
fn selected() -> Option<RuleEntry> {
    MANAGER.with(|slot| {
        let slot = slot.borrow();
        let m = slot.as_ref()?;
        let row = send(m.hwnd, LIST, LB_GETCURSEL, 0, 0) as usize;
        m.visible.get(row).and_then(|i| m.entries.get(*i)).cloned()
    })
}
fn show_selected() {
    let entry = selected();
    let Some((hwnd, _, _)) = context() else {
        return;
    };
    MANAGER.with(|slot| {
        if let Some(m) = slot.borrow_mut().as_mut() {
            m.pending = None;
        }
    });
    enable(hwnd, APPLY, false);
    enable(hwnd, SAVE, entry.is_some());
    enable(hwnd, DELETE, entry.is_some());
    enable(hwnd, MODE, entry.is_some());
    if let Some(entry) = entry {
        send(
            hwnd,
            MODE,
            CB_SETCURSEL,
            MODES.iter().position(|m| *m == entry.mode).unwrap_or(2),
            0,
        );
        let details = match &entry.scope {
            RuleScope::Global(_) => {
                "通用默认优先级最低；软件或控件规则可以覆盖它。恢复默认只修改这一项。".to_owned()
            }
            RuleScope::Application {
                control, element, ..
            } => {
                if let Some(e) = element {
                    format!("控件标识：{}\r\n类名：{}；控件类型：{}\r\n优先于软件区域、软件默认和通用默认。同一软件其他窗口的相同标识也会匹配。", e.automation_id, e.class_name, e.control_type)
                } else if control.is_some() {
                    "优先于软件默认和通用默认；更具体的控件规则仍优先。".into()
                } else {
                    "优先于通用默认；此软件已有的区域 / 控件规则仍优先。旧配置未设置全局终端或密码框时，相关安全兜底仍有效。".into()
                }
            }
        };
        set_text(
            hwnd,
            DETAIL,
            &format!(
                "{}\r\n目标：{}\r\n{}\r\n要检查某个输入框的最终命中规则，请使用“点选检查 / 新增”。",
                entry.scope.label(),
                entry.mode.description(),
                details
            ),
        );
        set_text(
            hwnd,
            DELETE,
            if matches!(entry.scope, RuleScope::Global(_)) {
                "恢复此项推荐默认"
            } else {
                "删除选中覆盖"
            },
        );
    } else {
        set_text(
            hwnd,
            DETAIL,
            "此筛选下没有规则。可以点选输入框新增，或选择其他筛选。",
        );
    }
}
fn refresh() -> Result<(), String> {
    let (_, store, _) = context().ok_or("窗口已关闭")?;
    let original = store.read_text()?;
    let entries = RuleEngine::from_text(&original)?.entries();
    MANAGER.with(|slot| {
        if let Some(m) = slot.borrow_mut().as_mut() {
            m.original = original;
            m.entries = entries;
            m.pending = None;
        }
    });
    fill_filters();
    Ok(())
}
fn edit(remove: bool) -> Result<(), String> {
    let entry = selected().ok_or("请先选择规则")?;
    let (hwnd, store, original) = context().ok_or("窗口已关闭")?;
    let mode = if remove {
        match entry.scope {
            RuleScope::Global(kind) => Some(match kind {
                ControlKind::Edit => TargetMode::NativeChinese,
                ControlKind::Other => TargetMode::Passthrough,
                _ => TargetMode::ImeEnglish,
            }),
            _ => None,
        }
    } else {
        Some(
            *MODES
                .get(send(hwnd, MODE, CB_GETCURSEL, 0, 0) as usize)
                .ok_or("请选择模式")?,
        )
    };
    store.save(&original, &entry.scope, mode)?;
    crate::mark_resync_after_enable();
    refresh()?;
    set_text(
        hwnd,
        STATUS,
        "已保存并加载，旧配置已备份。重新进入目标区域后使用新规则。",
    );
    Ok(())
}

pub fn describe_changes(before: &str, after: &str) -> Result<String, String> {
    let old = RuleEngine::from_text(before)?.entries();
    let new = RuleEngine::from_text(after)?.entries();
    let mut lines = Vec::new();
    for n in &new {
        match old.iter().find(|o| o.scope == n.scope) {
            Some(o) if o.mode == n.mode && o.configured == n.configured => {}
            Some(o) => lines.push(format!(
                "修改：{}：{}{} → {}{}",
                n.scope.label(),
                o.mode.description(),
                if o.configured { "" } else { "（兜底）" },
                n.mode.description(),
                if n.configured { "" } else { "（兜底）" }
            )),
            None => lines.push(format!(
                "新增：{} → {}",
                n.scope.label(),
                n.mode.description()
            )),
        }
    }
    for o in &old {
        if !new.iter().any(|n| n.scope == o.scope) {
            lines.push(format!(
                "删除：{} → {}",
                o.scope.label(),
                o.mode.description()
            ));
        }
    }
    if lines.is_empty() {
        lines.push("规则行为没有变化；应用后将替换配置文件文本。".into());
    }
    Ok(lines.join("\r\n"))
}
fn preview(title: &str, text: String) -> Result<(), String> {
    let (hwnd, _, original) = context().ok_or("窗口已关闭")?;
    let changes = describe_changes(&original, &text)?;
    set_text(hwnd, DETAIL, &format!("{title}\r\n\r\n{changes}"));
    set_text(
        hwnd,
        STATUS,
        "仅预览，尚未修改配置。点击“应用预览”后整体替换并备份旧配置；选择规则或刷新可取消。",
    );
    MANAGER.with(|slot| {
        if let Some(m) = slot.borrow_mut().as_mut() {
            m.pending = Some(Pending {
                text,
                expected: original,
            });
        }
    });
    for id in [SAVE, DELETE, MODE] {
        enable(hwnd, id, false);
    }
    enable(hwnd, APPLY, true);
    Ok(())
}
fn apply_pending() -> Result<(), String> {
    let (hwnd, store, _) = context().ok_or("窗口已关闭")?;
    let (expected, text) = MANAGER
        .with(|slot| {
            slot.borrow()
                .as_ref()?
                .pending
                .as_ref()
                .map(|p| (p.expected.clone(), p.text.clone()))
        })
        .ok_or("没有待应用预览")?;
    store.replace(&expected, &text)?;
    crate::mark_resync_after_enable();
    refresh()?;
    set_text(
        hwnd,
        STATUS,
        "预览已应用并加载。旧配置已备份，可用“恢复上一份预览”查看恢复内容。",
    );
    Ok(())
}
fn choose_file(hwnd: HWND, save: bool) -> Result<Option<PathBuf>, String> {
    let mut buffer = vec![0u16; 32768];
    if save {
        let name = wide("imeflow-rules.toml");
        buffer[..name.len()].copy_from_slice(&name);
    }
    let filter: Vec<u16> = "TOML 配置 (*.toml)\0*.toml\0所有文件\0*.*\0\0"
        .encode_utf16()
        .collect();
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(buffer.as_mut_ptr()),
        nMaxFile: buffer.len() as u32,
        lpstrDefExt: w!("toml"),
        Flags: OFN_EXPLORER
            | OFN_NOCHANGEDIR
            | OFN_PATHMUSTEXIST
            | if save {
                OFN_OVERWRITEPROMPT
            } else {
                OFN_FILEMUSTEXIST
            },
        ..Default::default()
    };
    let ok = unsafe {
        if save {
            GetSaveFileNameW(&mut dialog)
        } else {
            GetOpenFileNameW(&mut dialog)
        }
    }
    .as_bool();
    if !ok {
        let code = unsafe { CommDlgExtendedError() }.0;
        return if code == 0 {
            Ok(None)
        } else {
            Err(format!("文件对话框失败：{code}"))
        };
    }
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Ok(Some(PathBuf::from(String::from_utf16_lossy(
        &buffer[..end],
    ))))
}
fn transfer(import: bool) -> Result<(), String> {
    let (hwnd, store, _) = context().ok_or("窗口已关闭")?;
    let Some(path) = choose_file(hwnd, !import)? else {
        return Ok(());
    };
    if import {
        preview(
            &format!("导入预览：{}\r\n将替换全部配置，不是合并。", path.display()),
            std::fs::read_to_string(path).map_err(|e| e.to_string())?,
        )
    } else {
        if let Ok(target) = std::fs::canonicalize(&path) {
            if target == store.path() || target == store.path().with_extension("toml.bak") {
                return Err("导出请选择其他文件，不覆盖当前配置或恢复备份。".into());
            }
        }
        let text = store.read_text()?;
        RuleEngine::from_text(&text)?;
        std::fs::write(&path, text).map_err(|e| e.to_string())?;
        set_text(hwnd, STATUS, &format!("已导出当前配置：{}", path.display()));
        Ok(())
    }
}
fn command(id: usize) -> Result<(), String> {
    match id {
        SAVE => edit(false),
        DELETE => edit(true),
        REFRESH => refresh(),
        APPLY => apply_pending(),
        PICK => {
            let (_, store, _) = context().ok_or("窗口已关闭")?;
            crate::rule_panel::open(store, false)
        }
        IMPORT => transfer(true),
        EXPORT => transfer(false),
        RESTORE => {
            let (_, store, _) = context().ok_or("窗口已关闭")?;
            preview(
                "恢复上一份配置预览（应用时会备份当前配置）",
                store.backup_text()?,
            )
        }
        PREVIEW_PRESET => {
            let (hwnd, _, original) = context().ok_or("窗口已关闭")?;
            let index = send(hwnd, PRESET, CB_GETCURSEL, 0, 0) as usize;
            let (text, count) = apply_preset(&original, index)?;
            preview(&format!("预设预览：新增 {count} 条。已有同范围规则保留。\r\n新增区域规则会优先于软件 / 全局默认；控件规则仍优先。"), text)
        }
        _ => Ok(()),
    }
}
unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if let Some(result) = crate::settings_ui::paint(msg, wp, lp, &[SAVE, PICK, APPLY]) {
        return result;
    }
    match msg {
        WM_CLOSE => {
            close();
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = wp.0 & 0xffff;
            let event = wp.0 >> 16;
            if id == FILTER && event == CBN_SELCHANGE as usize {
                fill_rows();
            } else if id == LIST && event == LBN_SELCHANGE as usize {
                show_selected();
            } else if event == BN_CLICKED as usize {
                if let Err(e) = command(id) {
                    set_text(hwnd, STATUS, &format!("未完成：{e}"));
                }
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn send(hwnd: HWND, id: usize, message: u32, wp: usize, lp: isize) -> isize {
    unsafe {
        SendMessageW(
            GetDlgItem(hwnd, id as i32).unwrap_or_default(),
            message,
            WPARAM(wp),
            LPARAM(lp),
        )
        .0
    }
}
fn combo_add(hwnd: HWND, id: usize, label: &str) {
    let text = wide(label);
    send(hwnd, id, CB_ADDSTRING, 0, text.as_ptr() as isize);
}
fn set_text(hwnd: HWND, id: usize, label: &str) {
    let text = wide(label);
    unsafe {
        let _ = SetWindowTextW(
            GetDlgItem(hwnd, id as i32).unwrap_or_default(),
            PCWSTR(text.as_ptr()),
        );
    }
}
fn enable(hwnd: HWND, id: usize, value: bool) {
    unsafe {
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
            GetDlgItem(hwnd, id as i32).unwrap_or_default(),
            value,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filter_and_preview_use_exact_scopes() {
        let source = include_str!("../rules.toml");
        let entries = RuleEngine::from_text(source).unwrap().entries();
        assert_eq!(filter_entries(&entries, "").len(), 6);
        assert_eq!(filter_entries(&entries, "WECHAT.EXE").len(), 1);
        assert!(filter_entries(&entries, "chat.exe").is_empty());
        let (next, _) = apply_preset(source, 0).unwrap();
        let changes = describe_changes(source, &next).unwrap();
        assert!(changes.contains("新增：chrome.exe"));
        assert!(!changes.contains("删除："));
    }
    #[test]
    fn hidden_native_manager_saves_previews_and_rejects_stale_import() {
        let path =
            std::env::temp_dir().join(format!("imeflow-manager-{}.toml", std::process::id()));
        let source = include_str!("../rules.toml");
        std::fs::write(&path, source).unwrap();
        let store = Arc::new(RuleStore::load(&path).unwrap());
        open_inner(Arc::clone(&store), false).unwrap();
        let (hwnd, _, _) = context().unwrap();
        assert!(!unsafe { IsWindowVisible(hwnd) }.as_bool());
        send(hwnd, MODE, CB_SETCURSEL, 1, 0);
        edit(false).unwrap();
        assert_eq!(
            store.snapshot().global_mode(Some(ControlKind::Edit)),
            TargetMode::ImeEnglish
        );
        preview("restore", source.into()).unwrap();
        assert_eq!(
            store.snapshot().global_mode(Some(ControlKind::Edit)),
            TargetMode::ImeEnglish
        );
        apply_pending().unwrap();
        assert_eq!(
            store.snapshot().global_mode(Some(ControlKind::Edit)),
            TargetMode::NativeChinese
        );
        let filter_index = MANAGER.with(|slot| {
            slot.borrow()
                .as_ref()
                .unwrap()
                .filters
                .iter()
                .position(|p| p == "wechat.exe")
                .unwrap()
        });
        send(hwnd, FILTER, CB_SETCURSEL, filter_index, 0);
        fill_rows();
        assert_eq!(selected().unwrap().scope.process(), Some("wechat.exe"));
        edit(true).unwrap();
        assert!(!store
            .snapshot()
            .entries()
            .iter()
            .any(|e| e.scope.process() == Some("wechat.exe")));
        preview("stale", source.into()).unwrap();
        std::fs::write(&path, format!("{source}\n# external")).unwrap();
        assert!(apply_pending().is_err());
        close();
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(path.with_extension("toml.bak")).unwrap();
    }
}
