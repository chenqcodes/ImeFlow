//! IME mode requests. A Shift fallback is one pulse, never a retry loop.

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::Ime::{
    ImmGetContext, ImmGetConversionStatus, ImmGetDefaultIMEWnd, ImmGetOpenStatus,
    ImmReleaseContext, ImmSetConversionStatus, ImmSetOpenStatus, IME_CMODE_NATIVE,
    IME_CONVERSION_MODE, IME_SENTENCE_MODE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetGUIThreadInfo, GetWindowThreadProcessId, SendMessageTimeoutW, GUITHREADINFO,
    SMTO_ABORTIFHUNG, SMTO_BLOCK,
};

const IMC_GETCONVERSIONMODE: usize = 0x0001;
const IMC_SETCONVERSIONMODE: usize = 0x0002;
const IMC_GETOPENSTATUS: usize = 0x0005;
const IMC_SETOPENSTATUS: usize = 0x0006;
const WM_IME_CONTROL: u32 = 0x0283;

/// Diagnostic tag written to KEYBDINPUT.dwExtraInfo (`IMEF` ascii).
pub const IMEF_EXTRA: usize = 0x494D4546;

pub fn read_ime_status(hwnd: HWND) -> (Option<bool>, Option<bool>) {
    for candidate in target_hwnds(hwnd) {
        if let Some(value) = read_via_himc(candidate) {
            if value.1.is_some() { return value; }
        }
        let value = read_via_ime_wnd(candidate);
        if value.1.is_some() { return value; }
    }
    (None, None)
}

pub fn set_native_mode(hwnd: HWND, native: bool) -> Result<(), String> {
    let target = crate::process::InputTarget::capture(hwnd)?;
    // Do not trust a matching IMM read as proof of effective mode. We have observed
    // ChatGPT/Codex report native=true while the real edit path is still in IME English.
    let mut last_err = String::from("no conversion path");
    for candidate in target_hwnds(hwnd) {
        target.ensure_current()?;
        match set_via_himc(candidate, native) {
            Ok(()) if read_ime_status(hwnd).1 == Some(native) => return Ok(()),
            Ok(()) => {},
            Err(e) => last_err = e,
        }
        target.ensure_current()?;
        match set_via_ime_wnd(candidate, native) {
            Ok(()) if read_ime_status(hwnd).1 == Some(native) => return Ok(()),
            Ok(()) => {},
            Err(e) => last_err = e,
        }
    }
    target.ensure_current()?;
    tracing::debug!(%last_err, "IMM path not confirmed; considering one Shift only");
    toggle_via_shift(hwnd, native)
}

/// Integrated terminal → IME English: wait for hwndFocus quiet, then one Shift.
/// Caller must only reach here after a trusted backend plan says Shift.
pub fn terminal_pulse_ime_english(hwnd: HWND) -> Result<(), String> {
    let quiet = crate::process::wait_focus_quiet(
        hwnd,
        std::time::Duration::from_millis(120),
        std::time::Duration::from_millis(800),
    )?;
    quiet.ensure_current()?;
    tap_left_shift_scancode()?;
    tracing::info!(
        focus = format!("{:x}", quiet.focus as usize),
        "terminal: focus quiet → one Shift → expected 英; no IMM skip"
    );
    Ok(())
}

pub fn modifiers_down() -> bool {
    [VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN]
        .iter().any(|vk| unsafe { GetAsyncKeyState(vk.0 as i32) < 0 })
}

fn toggle_via_shift(hwnd: HWND, want_native: bool) -> Result<(), String> {
    let target = crate::process::InputTarget::capture(hwnd)?;
    let before = read_ime_status(hwnd).1;
    match before {
        Some(value) if value == want_native => {
            tracing::info!("shift: report already matches; effective_mode=unknown, no pulse");
            return Ok(());
        }
        None => return Err("shift skipped: unknown initial mode; cannot safely toggle".into()),
        _ => {},
    }
    target.ensure_current()?;
    tap_left_shift_scancode()?;
    tracing::info!(want_native, "shift: pulses=1, outcome=applied_unverified; no retry");
    // Do not inspect a potentially stale IMM value and toggle again.
    Ok(())
}

/// Inserts exactly one left-Shift down/up pair. No retry loop.
pub fn tap_left_shift_scancode() -> Result<(), String> {
    tap_scancode(0x2A)
}

/// Phase-1 letter probe (`b`). Opt-in only — will type into the focused control.
pub fn tap_probe_letter_b() -> Result<(), String> {
    tap_scancode(0x30)
}

fn key_input(scan: u16, up: bool) -> INPUT {
    let flags = if up {
        KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP
    } else {
        KEYEVENTF_SCANCODE
    };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: Default::default(),
                wScan: scan,
                dwFlags: flags,
                dwExtraInfo: IMEF_EXTRA,
                ..Default::default()
            },
        },
    }
}

fn tap_scancode(scan: u16) -> Result<(), String> {
    if modifiers_down() {
        return Err("key skipped: user is holding a modifier".into());
    }
    let up = key_input(scan, true);
    let sent = unsafe {
        SendInput(
            &[key_input(scan, false), up],
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    if sent != 2 {
        if sent == 1 {
            let released = unsafe { SendInput(&[up], std::mem::size_of::<INPUT>() as i32) };
            tracing::warn!(scan, released, "partial key insertion; release-only cleanup");
        }
        return Err(format!("SendInput scan={scan:#x} inserted={sent}/2"));
    }
    Ok(())
}

fn target_hwnds(hwnd: HWND) -> Vec<HWND> {
    let mut targets = Vec::new();
    if let Some(focus) = focus_hwnd_of(hwnd) {
        if focus.0 != hwnd.0 { targets.push(focus); }
    }
    targets.push(hwnd);
    targets
}

fn focus_hwnd_of(hwnd: HWND) -> Option<HWND> {
    let tid = unsafe { GetWindowThreadProcessId(hwnd, None) };
    if tid == 0 { return None; }
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetGUIThreadInfo(tid, &mut info).ok()?; }
    if info.hwndFocus.0.is_null() { None } else { Some(info.hwndFocus) }
}

fn read_via_himc(hwnd: HWND) -> Option<(Option<bool>, Option<bool>)> {
    unsafe {
        let himc = ImmGetContext(hwnd);
        if himc.is_invalid() { return None; }
        let open = ImmGetOpenStatus(himc).as_bool();
        let mut conv = IME_CONVERSION_MODE(0);
        let mut sent = IME_SENTENCE_MODE(0);
        let ok = ImmGetConversionStatus(himc, Some(&mut conv), Some(&mut sent)).as_bool();
        let _ = ImmReleaseContext(hwnd, himc);
        Some((Some(open), ok.then_some((conv.0 & IME_CMODE_NATIVE.0) != 0)))
    }
}

fn set_via_himc(hwnd: HWND, native: bool) -> Result<(), String> {
    unsafe {
        let himc = ImmGetContext(hwnd);
        if himc.is_invalid() { return Err("no HIMC".into()); }
        let result = apply_himc(himc, native);
        let _ = ImmReleaseContext(hwnd, himc);
        result
    }
}

fn ime_wnd(hwnd: HWND) -> Result<HWND, String> {
    let wnd = unsafe { ImmGetDefaultIMEWnd(hwnd) };
    if wnd.0.is_null() { Err("ImmGetDefaultIMEWnd null".into()) } else { Ok(wnd) }
}

fn ime_control(wnd: HWND, command: usize, value: isize) -> Result<usize, String> {
    let mut result = 0usize;
    let ok = unsafe {
        SendMessageTimeoutW(wnd, WM_IME_CONTROL, WPARAM(command), LPARAM(value),
            SMTO_ABORTIFHUNG | SMTO_BLOCK, 100, Some(&mut result))
    };
    if ok.0 == 0 { Err(format!("WM_IME_CONTROL {command:#x} failed/timed out")) }
    else { Ok(result) }
}

fn read_via_ime_wnd(hwnd: HWND) -> (Option<bool>, Option<bool>) {
    let Ok(wnd) = ime_wnd(hwnd) else { return (None, None) };
    let open = ime_control(wnd, IMC_GETOPENSTATUS, 0).ok().map(|v| v != 0);
    let native = ime_control(wnd, IMC_GETCONVERSIONMODE, 0).ok()
        .map(|v| (v as u32 & IME_CMODE_NATIVE.0) != 0);
    (open, native)
}

fn set_via_ime_wnd(hwnd: HWND, native: bool) -> Result<(), String> {
    let wnd = ime_wnd(hwnd)?;
    ime_control(wnd, IMC_SETOPENSTATUS, 1)?;
    let current = ime_control(wnd, IMC_GETCONVERSIONMODE, 0)? as u32;
    let next = if native { current | IME_CMODE_NATIVE.0 } else { current & !IME_CMODE_NATIVE.0 };
    ime_control(wnd, IMC_SETCONVERSIONMODE, next as isize)?;
    Ok(())
}

unsafe fn apply_himc(himc: windows::Win32::UI::Input::Ime::HIMC, native: bool) -> Result<(), String> {
    let _ = ImmSetOpenStatus(himc, true);
    let mut conv = IME_CONVERSION_MODE(0);
    let mut sent = IME_SENTENCE_MODE(0);
    if !ImmGetConversionStatus(himc, Some(&mut conv), Some(&mut sent)).as_bool() {
        return Err("ImmGetConversionStatus failed".into());
    }
    if native { conv.0 |= IME_CMODE_NATIVE.0; } else { conv.0 &= !IME_CMODE_NATIVE.0; }
    if !ImmSetConversionStatus(himc, conv, sent).as_bool() {
        return Err("ImmSetConversionStatus failed".into());
    }
    Ok(())
}
