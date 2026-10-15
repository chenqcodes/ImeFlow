//! User-opened rule editor. Capture is opt-in, background UIA work never activates the target.
use crate::{
    mode::TargetMode,
    process,
    rules::{ElementSelector, RuleEngine, RuleScope, RuleStore},
    uia::{self, ControlKind, FocusInfo},
};
use std::{
    cell::RefCell,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            CreateFontW, DeleteObject, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
            DEFAULT_PITCH, HFONT, OUT_DEFAULT_PRECIS,
        },
        UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
    },
};

const PICK: usize = 201;
const SCOPE: usize = 202;
const MODE: usize = 203;
const SAVE: usize = 204;
const REMOVE: usize = 205;
const SUMMARY: usize = 206;
const STATUS: usize = 207;
const DETAILS: usize = 208;
const MANAGE: usize = 209;
const RELATED: usize = 210;
const POLL: usize = 20;
const HIGHLIGHT_TIMER: usize = 21;
static CAPTURING: AtomicBool = AtomicBool::new(false);
thread_local! { static PANEL: RefCell<Option<Panel>> = const { RefCell::new(None) }; }

const MODES: [(TargetMode, &str); 4] = [
    (TargetMode::NativeChinese, "中文"),
    (TargetMode::ImeEnglish, "英文（当前输入法）"),
    (TargetMode::Passthrough, "不干预"),
    (TargetMode::LayoutEnglish, "ENG 美式键盘（高级）"),
];

#[derive(Clone)]
struct Captured {
    root: isize,
    process: String,
    focus: FocusInfo,
    selector: Option<ElementSelector>,
}

struct Panel {
    hwnd: HWND,
    font: HFONT,
    _heading: crate::settings_ui::Heading,
    store: Arc<RuleStore>,
    original: String,
    global: bool,
    captured: Option<Captured>,
    scopes: Vec<RuleScope>,
    worker_tx: mpsc::Sender<u64>,
    worker_rx: mpsc::Receiver<(u64, Option<Captured>)>,
    pending: bool,
    generation: u64,
    focus_generation: u64,
    highlight: Option<crate::highlight::Highlight>,
    highlight_until: Option<Instant>,
}

pub fn is_capturing() -> bool {
    CAPTURING.load(Ordering::SeqCst)
}

pub fn handle_message(message: &MSG) -> bool {
    let hwnd = PANEL.with(|cell| cell.borrow().as_ref().map(|p| p.hwnd));
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

fn capture() -> Option<Captured> {
    let root = process::root_hwnd(process::foreground_hwnd());
    if root.0.is_null() || process::hwnd_process_id(root) == std::process::id() {
        return None;
    }
    let native = process::InputTarget::capture(root).ok()?;
    let exe = process::hwnd_exe_name(root)?;
    let first = uia::inspect_focused()?;
    let key = crate::focus_key(root, &first)?;
    if first.is_navigation_control() {
        return None;
    }
    std::thread::sleep(Duration::from_millis(150));
    let focus = uia::inspect_focused()?;
    if crate::focus_key(root, &focus).as_deref() != Some(&key) || focus.is_navigation_control() {
        return None;
    }
    let selector = if uia::identifier_unique_in_window(root, &focus) {
        ElementSelector::from_focus(&focus)
    } else {
        None
    };
    native.ensure_current().ok()?;
    // A last identity read prevents an expensive provider query from pinning stale focus.
    if crate::focus_key(root, &uia::inspect_focused()?).as_deref() != Some(&key) {
        return None;
    }
    Some(Captured {
        root: native.root,
        process: exe,
        focus,
        selector,
    })
}

pub fn open(store: Arc<RuleStore>, global: bool) -> Result<(), String> {
    open_inner(store, global, true)
}

fn open_inner(store: Arc<RuleStore>, global: bool, visible: bool) -> Result<(), String> {
    close();
    let original = store.read_text()?;
    RuleEngine::from_text(&original)?;
    let (request_tx, request_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("rule-picker".into())
        .spawn(move || {
            while let Ok(generation) = request_rx.recv() {
                if result_tx.send((generation, capture())).is_err() {
                    break;
                }
            }
        })
        .map_err(|e| e.to_string())?;
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
            .map_err(|e| e.to_string())?;
        let class = w!("ImeFlowRulePanel");
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
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class,
            if global {
                w!("ImeFlow · 通用默认")
            } else {
                w!("ImeFlow · 点选设置")
            },
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            560,
            460,
            None,
            None,
            instance,
            None,
        )
        .map_err(|e| e.to_string())?;

        let mut work = RECT::default();
        let _ = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut work as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        let dpi = (GetDpiForWindow(hwnd).max(96) as i32)
            .min((work.right - work.left - 40) * 96 / 660)
            .min((work.bottom - work.top - 40) * 96 / 490)
            .max(48);
        let scale = |n: i32| n * dpi / 96;
        let width = scale(660);
        let height = scale(490);
        let heading = crate::settings_ui::Heading::new(dpi);
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            (work.right - width - scale(20)).max(work.left),
            (work.bottom - height - scale(20)).max(work.top),
            width,
            height,
            SWP_NOACTIVATE,
        );
        let font = CreateFontW(
            -scale(14),
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
        let make = |class: PCWSTR,
                    text: &str,
                    id: usize,
                    style: WINDOW_STYLE,
                    x,
                    y,
                    width,
                    height|
         -> Result<(), String> {
            let style = if class.to_string().is_ok_and(|name| name == "BUTTON") {
                style | WINDOW_STYLE(BS_OWNERDRAW as u32)
            } else {
                style
            };
            let text = wide(text);
            let child = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                PCWSTR(text.as_ptr()),
                WS_CHILD | WS_VISIBLE | style,
                scale(x),
                scale(y),
                scale(width),
                scale(height),
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
                if global {
                    "通用默认"
                } else {
                    "输入区域设置"
                },
                900,
                WINDOW_STYLE(0),
                20,
                16,
                420,
                30,
            )?;
            make(w!("STATIC"), "", SUMMARY, WINDOW_STYLE(0), 20, 62, 290, 100)?;
            make(
                w!("STATIC"),
                if global {
                    "输入区域"
                } else {
                    "生效范围"
                },
                0,
                WINDOW_STYLE(0),
                20,
                177,
                290,
                22,
            )?;
            make(
                w!("COMBOBOX"),
                "",
                SCOPE,
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                20,
                203,
                290,
                220,
            )?;
            make(
                w!("STATIC"),
                "输入模式",
                0,
                WINDOW_STYLE(0),
                20,
                244,
                290,
                22,
            )?;
            make(
                w!("COMBOBOX"),
                "",
                MODE,
                WINDOW_STYLE(CBS_DROPDOWNLIST as u32) | WS_TABSTOP | WS_VSCROLL,
                20,
                270,
                290,
                160,
            )?;
            make(w!("BUTTON"), "保存", SAVE, WS_TABSTOP, 20, 319, 92, 34)?;
            make(
                w!("BUTTON"),
                "删除此范围的覆盖",
                REMOVE,
                WS_TABSTOP,
                122,
                319,
                188,
                34,
            )?;
            make(w!("BUTTON"), "重新点选", PICK, WS_TABSTOP, 20, 365, 90, 30)?;
            make(
                w!("BUTTON"),
                "匹配说明",
                DETAILS,
                WS_TABSTOP,
                120,
                365,
                90,
                30,
            )?;
            make(
                w!("BUTTON"),
                "规则列表",
                MANAGE,
                WS_TABSTOP,
                220,
                365,
                90,
                30,
            )?;
            make(
                w!("STATIC"),
                "已有配置 · 优先级从上到下",
                0,
                WINDOW_STYLE(0),
                332,
                22,
                290,
                24,
            )?;
            make(
                w!("EDIT"),
                "点选输入区域后显示相关配置。",
                RELATED,
                WINDOW_STYLE(ES_MULTILINE as u32 | ES_READONLY as u32 | ES_AUTOVSCROLL as u32)
                    | WS_VSCROLL
                    | WS_TABSTOP,
                332,
                62,
                290,
                333,
            )?;
            make(w!("STATIC"), "", STATUS, WINDOW_STYLE(0), 20, 412, 602, 44)?;
            Ok::<_, String>(())
        })();
        if let Err(error) = result {
            let _ = DestroyWindow(hwnd);
            let _ = DeleteObject(font);
            return Err(error);
        }
        for (_, label) in MODES {
            combo_add(hwnd, MODE, label);
        }
        PANEL.with(|cell| {
            *cell.borrow_mut() = Some(Panel {
                hwnd,
                font,
                _heading: heading,
                store,
                original,
                global,
                captured: None,
                scopes: Vec::new(),
                worker_tx: request_tx,
                worker_rx: result_rx,
                pending: false,
                generation: 0,
                focus_generation: 0,
                highlight: None,
                highlight_until: None,
            })
        });
        if visible {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
        if global {
            set_text(hwnd, SUMMARY, "为所有软件设置通用默认。\r\n控件规则、软件区域规则和软件默认优先。\r\n按钮、菜单等非输入控件始终不触发切换。");
            let _ = ShowWindow(GetDlgItem(hwnd, PICK as i32).unwrap_or_default(), SW_HIDE);
            let _ = ShowWindow(
                GetDlgItem(hwnd, DETAILS as i32).unwrap_or_default(),
                SW_HIDE,
            );
            set_text(hwnd, REMOVE, "恢复推荐默认");
            PANEL.with(|cell| {
                let mut p = cell.borrow_mut();
                let p = p.as_mut().unwrap();
                for kind in ControlKind::ALL {
                    p.scopes.push(RuleScope::Global(kind));
                    combo_add(hwnd, SCOPE, kind.label());
                }
            });
            combo_select(hwnd, SCOPE, 0);
            update_mode();
        } else {
            arm();
        }
    }
    Ok(())
}

pub fn close() {
    CAPTURING.store(false, Ordering::SeqCst);
    let old = PANEL.with(|cell| cell.borrow_mut().take());
    if let Some(p) = old {
        unsafe {
            let _ = KillTimer(p.hwnd, POLL);
            let _ = KillTimer(p.hwnd, HIGHLIGHT_TIMER);
            let _ = DestroyWindow(p.hwnd);
            let _ = DeleteObject(p.font);
        }
        crate::mark_resync_after_enable();
    }
}

fn arm() {
    let hwnd = PANEL.with(|cell| {
        let mut p = cell.borrow_mut();
        let p = p.as_mut()?;
        p.captured = None;
        p.highlight = None;
        p.highlight_until = None;
        set_text(
            p.hwnd,
            RELATED,
            "等待点选输入区域…\r\n\r\n捕获后显示控件、软件区域、软件默认和全局默认。",
        );
        p.scopes.clear();
        p.generation += 1;
        Some(p.hwnd)
    });
    let Some(hwnd) = hwnd else {
        return;
    };
    CAPTURING.store(true, Ordering::SeqCst);
    unsafe {
        let _ = SetForegroundWindow(hwnd);
        SendMessageW(
            GetDlgItem(hwnd, SCOPE as i32).unwrap_or_default(),
            CB_RESETCONTENT,
            WPARAM(0),
            LPARAM(0),
        );
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
            GetDlgItem(hwnd, SAVE as i32).unwrap_or_default(),
            false,
        );
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
            GetDlgItem(hwnd, REMOVE as i32).unwrap_or_default(),
            false,
        );
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
            GetDlgItem(hwnd, DETAILS as i32).unwrap_or_default(),
            false,
        );
        let _ = SetTimer(hwnd, POLL, 200, None);
    }
    set_text(hwnd, SUMMARY, "请点击要设置的输入框。\r\n面板会锁定实际获得输入焦点的区域。\r\n点选期间暂不自动切换；关闭面板可取消。");
    set_text(
        hwnd,
        STATUS,
        "等待输入区域稳定。无法读取或点到按钮、菜单时会继续等待。\r\n面板不会显示或保存输入内容。",
    );
}

fn poll() {
    if !is_capturing() {
        return;
    }
    let captured = PANEL.with(|cell| {
        let mut p = cell.borrow_mut();
        let p = p.as_mut()?;
        if p.pending {
            match p.worker_rx.try_recv() {
                Ok((generation, result)) => {
                    p.pending = false;
                    if generation == p.generation
                        && p.focus_generation == crate::watcher::AppWatcher::generation()
                    {
                        result
                    } else {
                        None
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    p.pending = false;
                    None
                }
                Err(mpsc::TryRecvError::Empty) => None,
            }
        } else {
            p.focus_generation = crate::watcher::AppWatcher::generation();
            p.pending = p.worker_tx.send(p.generation).is_ok();
            None
        }
    });
    let Some(captured) = captured else {
        return;
    };
    let updated = PANEL.with(|cell| -> Result<HWND, String> {
        let mut p = cell.borrow_mut();
        let p = p.as_mut().ok_or("设置面板已关闭")?;
        p.original = p.store.read_text()?;
        RuleEngine::from_text(&p.original)?;
        p.scopes = capture_scopes(&captured);
        let hwnd = p.hwnd;
        unsafe {
            if captured.selector.is_some() {
                combo_add(hwnd, SCOPE, "此软件中相同标识的控件");
            }
            combo_add(hwnd, SCOPE, "此软件的同类区域");
            combo_add(hwnd, SCOPE, "整个软件（更具体规则仍优先）");
            combo_add(hwnd, SCOPE, "所有软件的此类区域（通用默认）");
            combo_select(hwnd, SCOPE, 0);
            let _ = KillTimer(hwnd, POLL);
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                GetDlgItem(hwnd, SAVE as i32).unwrap_or_default(),
                true,
            );
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                GetDlgItem(hwnd, REMOVE as i32).unwrap_or_default(),
                true,
            );
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                GetDlgItem(hwnd, DETAILS as i32).unwrap_or_default(),
                true,
            );
            if let Some(rect) = captured.focus.bounding_rect {
                p.highlight =
                    crate::highlight::Highlight::show(HWND(captured.root as *mut _), rect).ok();
                p.highlight_until = Some(Instant::now() + Duration::from_secs(3));
                let _ = SetTimer(hwnd, HIGHLIGHT_TIMER, 150, None);
            }
        }
        p.captured = Some(captured);
        Ok(hwnd)
    });
    CAPTURING.store(false, Ordering::SeqCst);
    crate::mark_resync_after_enable();
    match updated {
        Ok(_) => update_mode(),
        Err(e) => show_error(&e),
    }
}

fn update_highlight() {
    PANEL.with(|cell| {
        let mut p = cell.borrow_mut();
        let Some(p) = p.as_mut() else {
            return;
        };
        if p.highlight_until
            .is_none_or(|until| Instant::now() >= until)
            || p.highlight.as_ref().is_none_or(|h| !h.still_positioned())
        {
            p.highlight = None;
            p.highlight_until = None;
            unsafe {
                let _ = KillTimer(p.hwnd, HIGHLIGHT_TIMER);
            }
        }
    });
}

fn explain_match() {
    let detail = PANEL.with(|cell| {
        let p = cell.borrow();
        let p = p.as_ref()?;
        let captured = p.captured.as_ref()?;
        Some((
            p.hwnd,
            p.store
                .snapshot()
                .explain(&captured.process, &captured.focus),
        ))
    });
    if let Some((hwnd, text)) = detail {
        let text = wide(&text);
        unsafe {
            let _ = MessageBoxW(
                hwnd,
                PCWSTR(text.as_ptr()),
                w!("ImeFlow · 匹配说明"),
                MB_OK | MB_ICONINFORMATION,
            );
        }
    }
}

fn capture_scopes(c: &Captured) -> Vec<RuleScope> {
    let app = |control, element| RuleScope::Application {
        process: c.process.clone(),
        control,
        element,
    };
    let mut scopes = Vec::new();
    if let Some(selector) = &c.selector {
        scopes.push(app(Some(c.focus.kind), Some(selector.clone())));
    }
    scopes.push(app(Some(c.focus.kind), None));
    scopes.push(app(None, None));
    scopes.push(RuleScope::Global(c.focus.kind));
    scopes
}

fn update_mode() {
    PANEL.with(|cell| {
        let p = cell.borrow(); let Some(p) = p.as_ref() else { return; };
        let index = unsafe { combo_index(p.hwnd, SCOPE) };
        let Some(scope) = p.scopes.get(index) else { return; };
        let Ok(engine) = RuleEngine::from_text(&p.original) else { return; };
        if let Some(c) = &p.captured {
            let (current, _) = engine.resolve(&c.process, Some(c.focus.kind), Some(&c.focus));
            set_text(p.hwnd, RELATED, &engine.related_configuration(&c.process, &c.focus));
            let identifier = c.selector.as_ref().map(|s| s.automation_id.chars().take(32).collect::<String>()).unwrap_or_else(|| "无唯一标识".into());
            set_text(p.hwnd, SUMMARY, &format!("软件：{}\r\n区域：{}\r\n控件标识：{}\r\n当前目标：{}", c.process, c.focus.kind.label(), identifier, mode_label(current)));
        }
        if p.global {
            let rows = engine.entries().into_iter().filter(|e| matches!(e.scope, RuleScope::Global(_))).map(|e| format!("{}\r\n{}{}", e.scope.label(), e.mode.description(), if e.configured { " · 已配置" } else { " · 未配置，兜底" })).collect::<Vec<_>>();
            set_text(p.hwnd, RELATED, &rows.join("\r\n\r\n"));
        }
        let mode = match scope {
            RuleScope::Global(kind) => engine.global_mode(Some(*kind)),
            RuleScope::Application { process, control, element } => {
                let focus = if element.is_some() { p.captured.as_ref().map(|c| &c.focus) } else { None };
                engine.resolve(process, *control, focus).0
            }
        };
        unsafe { combo_select(p.hwnd, MODE, MODES.iter().position(|(m, _)| *m == mode).unwrap_or(2)); }
        set_text(p.hwnd, REMOVE, if matches!(scope, RuleScope::Global(_)) { "恢复推荐默认" } else { "删除此范围的覆盖" });
        set_text(p.hwnd, STATUS, if p.global {
            "修改后点击保存。更具体的手动规则仍会覆盖通用默认。\r\n代码区仅识别明确的代码结构；不按输入内容推测用途。"
        } else {
            "目标已锁定。保存后重新进入输入区域生效；删除覆盖后使用下一层规则。"
        });
    });
}

fn save(remove: bool) {
    let result = PANEL.with(|cell| -> Result<(), String> {
        let mut p = cell.borrow_mut();
        let p = p.as_mut().ok_or("面板已关闭")?;
        let scope = p
            .scopes
            .get(unsafe { combo_index(p.hwnd, SCOPE) })
            .ok_or("请先点选目标或选择输入区域")?;
        let mode = if remove {
            if let RuleScope::Global(kind) = scope {
                Some(recommended(*kind))
            } else {
                None
            }
        } else {
            Some(
                MODES
                    .get(unsafe { combo_index(p.hwnd, MODE) })
                    .ok_or("请选择模式")?
                    .0,
            )
        };
        p.store.save(&p.original, scope, mode)?;
        p.highlight = None;
        p.highlight_until = None;
        p.original = p.store.read_text()?;
        Ok(())
    });
    match result {
        Ok(()) => {
            crate::mark_resync_after_enable();
            update_mode();
            PANEL.with(|cell| {
                let p = cell.borrow(); let Some(p) = p.as_ref() else { return; };
                let suffix = if let Some(c) = &p.captured {
                    let engine = p.store.snapshot();
                    let (mode, source) = engine.resolve(&c.process, Some(c.focus.kind), Some(&c.focus));
                    format!("\r\n选中目标现在使用：{}（{}）。", mode_label(mode), source)
                } else { String::new() };
                set_text(p.hwnd, STATUS, &format!("已保存并加载。重新进入目标输入区域后生效。{}\r\n上一份配置已备份为 rules.toml.bak。", suffix));
            });
        }
        Err(e) => show_error(&e),
    }
}

fn recommended(kind: ControlKind) -> TargetMode {
    match kind {
        ControlKind::Edit => TargetMode::NativeChinese,
        ControlKind::Other => TargetMode::Passthrough,
        _ => TargetMode::ImeEnglish,
    }
}
fn mode_label(mode: TargetMode) -> &'static str {
    MODES
        .iter()
        .find(|(m, _)| *m == mode)
        .map(|(_, s)| *s)
        .unwrap_or("不干预")
}
fn show_error(error: &str) {
    let hwnd = PANEL.with(|cell| cell.borrow().as_ref().map(|p| p.hwnd));
    if let Some(hwnd) = hwnd {
        set_text(hwnd, STATUS, &format!("未保存：{error}"));
    }
}
unsafe extern "system" fn wnd_proc(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if let Some(result) = crate::settings_ui::paint(message, wp, lp, &[SAVE]) {
        return result;
    }
    match message {
        WM_CLOSE => {
            close();
            LRESULT(0)
        }
        WM_TIMER if wp.0 == POLL => {
            poll();
            LRESULT(0)
        }
        WM_TIMER if wp.0 == HIGHLIGHT_TIMER => {
            update_highlight();
            LRESULT(0)
        }
        WM_COMMAND => {
            match wp.0 & 0xffff {
                PICK if (wp.0 >> 16) == BN_CLICKED as usize => arm(),
                SAVE if (wp.0 >> 16) == BN_CLICKED as usize => save(false),
                REMOVE if (wp.0 >> 16) == BN_CLICKED as usize => save(true),
                DETAILS if (wp.0 >> 16) == BN_CLICKED as usize => explain_match(),
                MANAGE if (wp.0 >> 16) == BN_CLICKED as usize => {
                    let store =
                        PANEL.with(|cell| cell.borrow().as_ref().map(|p| Arc::clone(&p.store)));
                    if let Some(store) = store {
                        if let Err(error) = crate::rule_manager::open(store) {
                            show_error(&error);
                        }
                    }
                }
                SCOPE if (wp.0 >> 16) == CBN_SELCHANGE as usize => update_mode(),
                _ => {}
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wp, lp),
    }
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn set_text(hwnd: HWND, id: usize, text: &str) {
    let text = wide(text);
    unsafe {
        if let Ok(child) = GetDlgItem(hwnd, id as i32) {
            let _ = SetWindowTextW(child, PCWSTR(text.as_ptr()));
        }
    }
}
unsafe fn combo_add(hwnd: HWND, id: usize, text: &str) {
    let text = wide(text);
    SendMessageW(
        GetDlgItem(hwnd, id as i32).unwrap_or_default(),
        CB_ADDSTRING,
        WPARAM(0),
        LPARAM(text.as_ptr() as isize),
    );
}
unsafe fn combo_select(hwnd: HWND, id: usize, index: usize) {
    SendMessageW(
        GetDlgItem(hwnd, id as i32).unwrap_or_default(),
        CB_SETCURSEL,
        WPARAM(index),
        LPARAM(0),
    );
}
unsafe fn combo_index(hwnd: HWND, id: usize) -> usize {
    SendMessageW(
        GetDlgItem(hwnd, id as i32).unwrap_or_default(),
        CB_GETCURSEL,
        WPARAM(0),
        LPARAM(0),
    )
    .0 as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Manual screenshot fixture: isolated temporary rules, no IME hooks or user config writes.
    #[test]
    #[ignore]
    fn visual_review_settings_windows() {
        unsafe {
            let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
                windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            );
        }
        let path = std::env::temp_dir().join(format!("imeflow-visual-{}.toml", std::process::id()));
        std::fs::write(&path, include_str!("../rules.toml")).unwrap();
        let store = Arc::new(RuleStore::load(&path).unwrap());
        crate::rule_manager::open(Arc::clone(&store)).unwrap();
        open_inner(store, true, true).unwrap();
        PANEL.with(|slot| {
            let mut slot = slot.borrow_mut();
            let p = slot.as_mut().unwrap();
            p.global = false;
            let c = Captured {
                root: 0,
                process: "chrome.exe".into(),
                focus: FocusInfo {
                    kind: ControlKind::Edit,
                    automation_id: "compose-input".into(),
                    control_type: 50004,
                    class_name: "TextBox".into(),
                    ..Default::default()
                },
                selector: None,
            };
            p.captured = Some(c);
            unsafe {
                let _ = ShowWindow(GetDlgItem(p.hwnd, PICK as i32).unwrap(), SW_SHOW);
            }
        });
        update_mode();
        unsafe {
            for class in [w!("ImeFlowRulePanel"), w!("ImeFlowRuleManager")] {
                if let Ok(hwnd) = FindWindowW(class, None) {
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, WS_EX_APPWINDOW.0 as isize);
                    let _ = SetWindowPos(hwnd, HWND_NOTOPMOST, 80, 80, 0, 0, SWP_NOSIZE);
                    let _ = ShowWindow(hwnd, SW_HIDE);
                    let _ = ShowWindow(hwnd, SW_SHOW);
                }
            }
        }
        let until = Instant::now() + Duration::from_secs(180);
        unsafe {
            let mut msg = MSG::default();
            while Instant::now() < until {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if !handle_message(&msg) && !crate::rule_manager::handle_message(&msg) {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        }
        close();
        crate::rule_manager::close();
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("toml.bak"));
    }
    #[test]
    fn missing_identifier_does_not_offer_element_scope() {
        let c = Captured {
            root: 0,
            process: "demo.exe".into(),
            focus: FocusInfo {
                kind: ControlKind::Edit,
                ..Default::default()
            },
            selector: None,
        };
        let scopes = capture_scopes(&c);
        assert_eq!(scopes.len(), 3);
        assert!(matches!(
            &scopes[0],
            RuleScope::Application {
                control: Some(ControlKind::Edit),
                element: None,
                ..
            }
        ));
    }
    #[test]
    fn bundled_defaults_match_panel_reset_values() {
        let engine = RuleEngine::from_text(include_str!("../rules.toml")).unwrap();
        for kind in ControlKind::ALL {
            assert_eq!(engine.global_mode(Some(kind)), recommended(kind));
        }
    }

    #[test]
    fn native_global_panel_saves_and_restores_without_starting_ime_hooks() {
        let path =
            std::env::temp_dir().join(format!("imeflow-native-panel-{}.toml", std::process::id()));
        std::fs::write(&path, include_str!("../rules.toml")).unwrap();
        let store = Arc::new(RuleStore::load(&path).unwrap());
        open_inner(Arc::clone(&store), true, false).unwrap();
        let hwnd = PANEL.with(|cell| cell.borrow().as_ref().unwrap().hwnd);
        unsafe {
            assert!(!IsWindowVisible(hwnd).as_bool());
            assert_eq!(combo_index(hwnd, MODE), 0);
            combo_select(hwnd, MODE, 1);
            SendMessageW(hwnd, WM_COMMAND, WPARAM(SAVE), LPARAM(0));
        }
        assert_eq!(
            store.snapshot().global_mode(Some(ControlKind::Edit)),
            TargetMode::ImeEnglish
        );
        unsafe {
            SendMessageW(hwnd, WM_COMMAND, WPARAM(REMOVE), LPARAM(0));
        }
        assert_eq!(
            store.snapshot().global_mode(Some(ControlKind::Edit)),
            TargetMode::NativeChinese
        );
        close();
        assert!(!is_capturing());
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(path.with_extension("toml.bak")).unwrap();
    }
}
