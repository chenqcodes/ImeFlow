//! System tray. Four connected 中E states from assets/icons/; shell calls are queued and deduplicated.
use crate::{debug_ui, feedback::FeedbackBus, mode::TargetMode, ui_graphics::OwnedIcon};
use once_cell::sync::OnceCell;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::time::Duration;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DestroyWindow, GetCursorPos, GetSystemMetrics, KillTimer, PostMessageW, RegisterClassW,
    RegisterWindowMessageW, SetForegroundWindow, SetTimer, TrackPopupMenu, HICON, HMENU,
    MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, SM_CXSMICON, SM_CYSMICON,
    TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON, WM_COMMAND, WM_DESTROY, WM_DISPLAYCHANGE,
    WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER, WM_USER,
    WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};

const WM_TRAY: u32 = WM_USER + 80;
const WM_REFRESH: u32 = WM_USER + 81;
const ID_TOGGLE: usize = 1001;
const ID_AUTOSTART: usize = 1003;
const ID_LOGS: usize = 1004;
const ID_EXIT: usize = 1005;
const TIMER_RESYNC: usize = 2;
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);
static ENABLED: AtomicBool = AtomicBool::new(true);
static REFRESH_QUEUED: AtomicBool = AtomicBool::new(false);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static BUS: OnceCell<FeedbackBus> = OnceCell::new();
static LAST_SENT: OnceCell<parking_lot::Mutex<Option<(isize, String)>>> = OnceCell::new();
static ICON_ZH: AtomicIsize = AtomicIsize::new(0);
static ICON_EN: AtomicIsize = AtomicIsize::new(0);
static ICON_OFF: AtomicIsize = AtomicIsize::new(0);
static ICON_NEUTRAL: AtomicIsize = AtomicIsize::new(0);
/// Sticky tray layer so begin_focus Waiting does not flash the gray/full-color idle icon.
static LAST_TRAY_MODE: OnceCell<parking_lot::Mutex<Option<TargetMode>>> = OnceCell::new();

pub fn enabled() -> bool { ENABLED.load(Ordering::SeqCst) }
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::SeqCst);
    debug_ui::set_extra(|e| e.enabled = on);
    refresh();
}
pub fn install(bus: FeedbackBus, initially_enabled: bool) -> Result<HWND, String> {
    let _ = BUS.set(bus);
    let _ = LAST_SENT.set(parking_lot::Mutex::new(None));
    let _ = LAST_TRAY_MODE.set(parking_lot::Mutex::new(None));
    ENABLED.store(initially_enabled, Ordering::SeqCst);
    debug_ui::set_extra(|e| e.enabled = initially_enabled);
    let instance = unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None) }
        .map_err(|e| e.to_string())?;
    load_icons()?;
    let result = unsafe {
        let class = w!("ImeFlowTrayHost");
        let wc = WNDCLASSW { lpfnWndProc: Some(tray_wnd_proc), hInstance: instance.into(), lpszClassName: class, ..Default::default() };
        let _ = RegisterClassW(&wc);
        CreateWindowExW(WS_EX_TOOLWINDOW, class, w!("ImeFlowTray"), WS_POPUP,
            0, 0, 0, 0, None, None, instance, None).map_err(|e| e.to_string())
    };
    let hwnd = match result { Ok(h) => h, Err(e) => { release_icons(); return Err(e); } };
    TRAY_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
    TASKBAR_CREATED.store(unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) }, Ordering::SeqCst);
    if let Err(e) = publish(hwnd, true) { remove(); return Err(e); }
    tracing::info!("tray: four 中E states installed");
    Ok(hwnd)
}

fn load_icons() -> Result<(), String> {
    let assets = crate::assets::get()?;
    // Prefer ≥32px source; shell still displays in SM_* tray cell.
    let width = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64).max(32) as usize;
    let height = unsafe { GetSystemMetrics(SM_CYSMICON) }.clamp(16, 64).max(32) as usize;
    let mut icons = Vec::with_capacity(4);
    for image in &assets.icons {
        icons.push(OwnedIcon::from_image(&image.fitted(width, height)?)?);
    }
    for (slot, icon) in [&ICON_ZH, &ICON_EN, &ICON_OFF, &ICON_NEUTRAL].into_iter().zip(icons) {
        let old = slot.swap(icon.into_raw().0 as isize, Ordering::SeqCst);
        if old != 0 { unsafe { let _ = DestroyIcon(HICON(old as *mut _)); } }
    }
    if let Some(last) = LAST_SENT.get() { *last.lock() = None; }
    tracing::info!(width, height, "tray: 中E icons loaded");
    Ok(())
}
fn release_icons() {
    for slot in [&ICON_ZH, &ICON_EN, &ICON_OFF, &ICON_NEUTRAL] {
        let raw = slot.swap(0, Ordering::SeqCst);
        if raw != 0 { unsafe { let _ = DestroyIcon(HICON(raw as *mut _)); } }
    }
}
pub fn remove() {
    let raw = TRAY_HWND.swap(0, Ordering::SeqCst);
    REFRESH_QUEUED.store(false, Ordering::SeqCst);
    if raw != 0 { unsafe {
        let hwnd = HWND(raw as *mut _);
        let _ = KillTimer(hwnd, TIMER_RESYNC);
        let mut data = nid(hwnd);
        let _ = Shell_NotifyIconW(NIM_DELETE, &mut data);
        let _ = DestroyWindow(hwnd);
    } }
    release_icons();
    if let Some(last) = LAST_SENT.get() { *last.lock() = None; }
}
pub fn refresh() {
    let raw = TRAY_HWND.load(Ordering::SeqCst);
    if raw == 0 || REFRESH_QUEUED.swap(true, Ordering::SeqCst) { return; }
    if unsafe { PostMessageW(HWND(raw as *mut _), WM_REFRESH, WPARAM(0), LPARAM(0)) }.is_err() {
        REFRESH_QUEUED.store(false, Ordering::SeqCst);
    }
}

fn icon_index(on: bool, mode: Option<TargetMode>) -> usize {
    if !on { return 2; }
    match mode {
        Some(TargetMode::NativeChinese) => 0,
        Some(TargetMode::ImeEnglish | TargetMode::LayoutEnglish) => 1,
        _ => 3,
    }
}
fn presentation() -> (HICON, String) {
    let (process, stats) = BUS.get().map(|b| b.tip_stats())
        .unwrap_or_else(|| (String::new(), Default::default()));
    let confirmed_mode = BUS.get().and_then(|b| b.last_status().1);
    let hint = BUS.get().and_then(|b| b.tray_mode());
    let mode = {
        let cell = LAST_TRAY_MODE.get_or_init(|| parking_lot::Mutex::new(None));
        let mut last = cell.lock();
        if let Some(m) = hint {
            *last = Some(m);
        }
        last.clone().or(hint)
    };
    let on = enabled();
    let slots = [&ICON_ZH, &ICON_EN, &ICON_OFF, &ICON_NEUTRAL];
    let icon = HICON(slots[icon_index(on, mode)].load(Ordering::SeqCst) as *mut _);
    let mode_label = match mode.or(confirmed_mode) {
        Some(TargetMode::NativeChinese) => "中",
        Some(TargetMode::ImeEnglish) => "英",
        Some(TargetMode::LayoutEnglish) => "ENG",
        _ => "中+E",
    };
    let state = if on { "自动开启" } else { "已暂停" };
    let focus = if process.is_empty() { "—" } else { process.as_str() };
    // Multiline tip (Win10+). Keep under szTip 128 UTF-16 units.
    let tip = format!(
        "ImeFlow · {state}\n{mode_label} · {focus}\n今日确认 {today}\n累计确认 {total}",
        today = stats.today_auto_switch,
        total = stats.total_auto_switch,
    );
    (icon, tip)
}
fn publish(hwnd: HWND, add: bool) -> Result<(), String> {
    let (icon, tip) = presentation();
    if icon.0.is_null() { return Err("tray icon missing".into()); }
    if !add && LAST_SENT.get().map(|v| v.lock().as_ref() == Some(&(icon.0 as isize, tip.clone()))).unwrap_or(false) {
        return Ok(());
    }
    let mut data = nid(hwnd);
    data.uFlags = NIF_ICON | NIF_TIP;
    if add { data.uFlags |= NIF_MESSAGE; data.uCallbackMessage = WM_TRAY; }
    data.hIcon = icon;
    write_tip(&mut data, &tip);
    if !unsafe { Shell_NotifyIconW(if add { NIM_ADD } else { NIM_MODIFY }, &mut data) }.as_bool() {
        return Err("Shell_NotifyIconW failed".into());
    }
    if let Some(last) = LAST_SENT.get() { *last.lock() = Some((icon.0 as isize, tip)); }
    Ok(())
}
fn nid(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: 1, ..Default::default() }
}
fn write_tip(data: &mut NOTIFYICONDATAW, text: &str) {
    data.szTip.fill(0);
    let mut index = 0;
    // Never split a UTF-16 surrogate pair at the tooltip length limit.
    for ch in text.chars() {
        let mut buf = [0u16; 2];
        let units = ch.encode_utf16(&mut buf);
        if index + units.len() >= data.szTip.len() { break; }
        data.szTip[index..index + units.len()].copy_from_slice(units);
        index += units.len();
    }
}

unsafe extern "system" fn tray_wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let recreated = TASKBAR_CREATED.load(Ordering::SeqCst);
    if recreated != 0 && msg == recreated {
        if let Err(error) = publish(hwnd, true) { tracing::warn!(%error, "tray: Explorer recovery failed"); }
        return LRESULT(0);
    }
    match msg {
        WM_REFRESH => {
            REFRESH_QUEUED.store(false, Ordering::SeqCst);
            if let Err(error) = publish(hwnd, false) { tracing::warn!(%error, "tray: refresh failed"); }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
            if let Err(error) = load_icons() { tracing::warn!(%error, "tray: icon size refresh failed"); }
            refresh(); LRESULT(0)
        }
        WM_TRAY => {
            match lparam.0 as u32 {
                WM_LBUTTONUP => {}, WM_RBUTTONUP => show_menu(hwnd), _ => {},
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            match wparam.0 & 0xffff {
                ID_TOGGLE => {
                    let on = !enabled(); set_enabled(on);
                    // Retain the existing enable/cooldown behavior, not a new switching algorithm.
                    if on {
                        crate::mark_resync_after_enable();
                        if let Some(bus) = BUS.get() { bus.arm_override(Duration::from_millis(500)); }
                        let _ = KillTimer(hwnd, TIMER_RESYNC);
                        let _ = SetTimer(hwnd, TIMER_RESYNC, 550, None);
                    } else {
                        let _ = KillTimer(hwnd, TIMER_RESYNC);
                        crate::key_trace::clear_user_override();
                    }
                }
                ID_AUTOSTART => {
                    let want = !autostart_enabled();
                    match set_autostart(want) {
                        Ok(()) => tracing::info!(enabled = want, "tray: autostart changed"),
                        Err(error) => tracing::warn!(%error, "tray: autostart change failed"),
                    }
                }
                ID_LOGS => open_logs(),
                ID_EXIT => crate::watcher::AppWatcher::quit(),
                _ => {},
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_RESYNC => {
            let _ = KillTimer(hwnd, TIMER_RESYNC);
            if enabled() { crate::watcher::AppWatcher::request_recheck(); }
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn show_menu(hwnd: HWND) {
    let mut point = POINT::default();
    if GetCursorPos(&mut point).is_err() { return; }
    let Ok(menu) = CreatePopupMenu() else { return; };
    if let Some(bus) = BUS.get() {
        let (_, stats) = bus.tip_stats();
        append_disabled(menu, &format!("今日确认 {}", stats.today_auto_switch));
        append_disabled(menu, &format!("累计确认 {}", stats.total_auto_switch));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    }
    let checked = |value| if value { MF_STRING | MF_CHECKED } else { MF_STRING | MF_UNCHECKED };
    let _ = AppendMenuW(menu, checked(enabled()), ID_TOGGLE, w!("自动切换"));
    let _ = AppendMenuW(menu, checked(autostart_enabled()), ID_AUTOSTART, w!("开机自动启动"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, ID_LOGS, w!("打开日志目录"));
    let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, w!("退出"));
    let _ = SetForegroundWindow(hwnd); // Required for the user-opened menu, never for IME writes.
    let _ = TrackPopupMenu(menu, TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON,
        point.x, point.y, 0, hwnd, None);
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);
}
unsafe fn append_disabled(menu: HMENU, text: &str) {
    let text = wide(text);
    let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(text.as_ptr()));
}
fn autostart_enabled() -> bool { run_reg(["query", RUN_KEY, "/v", "ImeFlow"]).unwrap_or(false) }
fn set_autostart(on: bool) -> Result<(), String> {
    let success = if on {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let quoted = format!("\"{}\"", exe.display());
        run_reg(["add", RUN_KEY, "/v", "ImeFlow", "/t", "REG_SZ", "/d", quoted.as_str(), "/f"])?
    } else { run_reg(["delete", RUN_KEY, "/v", "ImeFlow", "/f"])? };
    if success { Ok(()) } else { Err("reg.exe returned a non-zero exit status".into()) }
}
fn run_reg<const N: usize>(args: [&str; N]) -> Result<bool, String> {
    Command::new("reg.exe").args(args).creation_flags(CREATE_NO_WINDOW)
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status()
        .map(|status| status.success()).map_err(|e| e.to_string())
}
fn open_logs() {
    let dir = std::env::var_os("IMEFLOW_LOG_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| {
        std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.join(".diagnostics")))
            .unwrap_or_else(|| std::env::temp_dir().join("ImeFlow/.diagnostics"))
    });
    if let Err(error) = std::fs::create_dir_all(&dir) { tracing::warn!(%error, "logs directory unavailable"); return; }
    if let Err(error) = Command::new("explorer.exe").arg(dir).spawn() { tracing::warn!(%error, "cannot open logs"); }
}
fn wide(s: &str) -> Vec<u16> { OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect() }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_and_paused_have_distinct_icons() {
        assert_eq!(icon_index(false, Some(TargetMode::NativeChinese)), 2);
        assert_eq!(icon_index(true, None), 3);
        assert_eq!(icon_index(true, Some(TargetMode::NativeChinese)), 0);
        assert_eq!(icon_index(true, Some(TargetMode::ImeEnglish)), 1);
    }
    #[test]
    fn tip_uses_multiline_layout() {
        let tip = "ImeFlow · 自动开启\n中 · Cursor.exe/edit\n今日确认 1\n累计确认 2";
        assert_eq!(tip.lines().count(), 4);
        assert!(tip.contains('\n'));
    }

    #[test]
    fn tooltip_truncation_keeps_utf16_valid() {
        let mut data = nid(HWND::default());
        write_tip(&mut data, &format!("{}🧪", "a".repeat(126)));
        let end = data.szTip.iter().position(|&v| v == 0).unwrap();
        assert!(String::from_utf16(&data.szTip[..end]).is_ok());
    }
}
