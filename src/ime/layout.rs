use super::tsf;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ActivateKeyboardLayout, GetKeyboardLayout, GetKeyboardLayoutList, LoadKeyboardLayoutW, HKL,
    KLF_ACTIVATE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowThreadProcessId, PostMessageW, SendMessageTimeoutW, SMTO_ABORTIFHUNG,
    WM_INPUTLANGCHANGEREQUEST,
};
use windows::core::PCWSTR;

const LANG_EN_US: u16 = 0x0409;
const LANG_ZH_CN: u16 = 0x0804;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveIme {
    LayoutEnglish,
    WeChatIme,
    MsPinyin,
    OtherChinese,
    Unknown,
}

impl ActiveIme {
    pub fn label(self) -> &'static str {
        match self {
            Self::LayoutEnglish => "ENG-layout",
            Self::WeChatIme => "WeChatIME",
            Self::MsPinyin => "MsPinyin",
            Self::OtherChinese => "ZhIME",
            Self::Unknown => "Unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LayoutInfo {
    pub lang_id: u16,
    pub backend: ActiveIme,
}

pub fn detect_backend(hkl: HKL) -> ActiveIme {
    // Prefer TSF active tip — HKL alone cannot distinguish WeChat vs MsPinyin.
    if let Some(tip) = tsf::active_keyboard_profile() {
        return tip.backend;
    }

    let value = hkl.0 as usize;
    let lang = (value & 0xFFFF) as u16;
    if lang == LANG_EN_US {
        return ActiveIme::LayoutEnglish;
    }

    let name = hkl_display_name(hkl).unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    if lower.contains("微信") || lower.contains("wechat") || lower.contains("weixin") {
        return ActiveIme::WeChatIme;
    }
    if lower.contains("microsoft pinyin")
        || lower.contains("微软拼音")
        || lower.contains("mspinyin")
        || (name.contains("微软") && name.contains("拼音"))
    {
        return ActiveIme::MsPinyin;
    }
    if lang == LANG_ZH_CN || (lang & 0xFF) == 0x04 {
        return ActiveIme::OtherChinese;
    }
    ActiveIme::Unknown
}

fn hkl_display_name(hkl: HKL) -> Option<String> {
    use windows::Win32::UI::Input::Ime::ImmGetDescriptionW;
    let mut buf = [0u16; 256];
    let len = unsafe { ImmGetDescriptionW(hkl, Some(&mut buf)) };
    if len == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

pub fn read_layout(hwnd: HWND) -> LayoutInfo {
    let tid = unsafe { GetWindowThreadProcessId(hwnd, None) };
    let hkl = unsafe { GetKeyboardLayout(tid) };
    let lang_id = (hkl.0 as usize & 0xFFFF) as u16;
    let backend = detect_backend(hkl);
    LayoutInfo { lang_id, backend }
}

fn load_hkl(klid: &str) -> windows::core::Result<HKL> {
    let wide: Vec<u16> = klid.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { LoadKeyboardLayoutW(PCWSTR(wide.as_ptr()), KLF_ACTIVATE) }
}

fn with_attached_input<T>(hwnd: HWND, f: impl FnOnce() -> T) -> T {
    let target_tid = unsafe { GetWindowThreadProcessId(hwnd, None) };
    let self_tid = unsafe { GetCurrentThreadId() };
    let attached = if target_tid != 0 && target_tid != self_tid {
        unsafe { AttachThreadInput(self_tid, target_tid, true).as_bool() }
    } else {
        false
    };
    let result = f();
    if attached {
        unsafe {
            let _ = AttachThreadInput(self_tid, target_tid, false);
        }
    }
    result
}

fn post_layout_change(hwnd: HWND, hkl: HKL) {
    // Also nudge focus child + Chromium render host (terminals / Electron).
    let mut targets = vec![hwnd];
    if let Some(focus) = focus_hwnd_of(hwnd) {
        if focus.0 != hwnd.0 {
            targets.push(focus);
        }
    }
    if let Some(child) = chrome_child(hwnd) {
        targets.push(child);
    }

    for target in targets {
        with_attached_input(target, || unsafe {
            // Do NOT KLF_SETFORPROCESS — that only changes ImeFlow itself.
            let _ = ActivateKeyboardLayout(hkl, Default::default());
            let _ = PostMessageW(
                target,
                WM_INPUTLANGCHANGEREQUEST,
                WPARAM(1), // INPUTLANGCHANGE_SYSCHARSET
                LPARAM(hkl.0 as isize),
            );
            let mut result = 0usize;
            let _ = SendMessageTimeoutW(
                target,
                WM_INPUTLANGCHANGEREQUEST,
                WPARAM(1),
                LPARAM(hkl.0 as isize),
                SMTO_ABORTIFHUNG,
                200,
                Some(&mut result),
            );
        });
    }
}

fn focus_hwnd_of(hwnd: HWND) -> Option<HWND> {
    use windows::Win32::UI::WindowsAndMessaging::{GetGUIThreadInfo, GUITHREADINFO};
    let tid = unsafe { GetWindowThreadProcessId(hwnd, None) };
    if tid == 0 {
        return None;
    }
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        GetGUIThreadInfo(tid, &mut info).ok()?;
    }
    if info.hwndFocus.0.is_null() {
        None
    } else {
        Some(info.hwndFocus)
    }
}

fn chrome_child(hwnd: HWND) -> Option<HWND> {
    use windows::Win32::Foundation::{BOOL, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetClassNameW};
    struct State {
        found: HWND,
    }
    unsafe extern "system" fn proc(child: HWND, lp: LPARAM) -> BOOL {
        let state = &mut *(lp.0 as *mut State);
        let mut buf = [0u16; 128];
        let n = GetClassNameW(child, &mut buf);
        if n > 0 {
            let class = String::from_utf16_lossy(&buf[..n as usize]);
            if class.contains("Chrome_RenderWidgetHostHWND")
                || class.contains("Chrome_WidgetWin")
                || class.contains("TermControl")
            {
                state.found = child;
                return BOOL(0);
            }
        }
        BOOL(1)
    }
    let mut state = State {
        found: HWND::default(),
    };
    unsafe {
        let _ = EnumChildWindows(hwnd, Some(proc), LPARAM(&mut state as *mut _ as isize));
    }
    if state.found.0.is_null() {
        None
    } else {
        Some(state.found)
    }
}

pub fn activate_english_layout(hwnd: HWND) -> Result<(), String> {
    let hkl = load_hkl("00000409").map_err(|e| format!("LoadKeyboardLayout en-US: {e}"))?;
    // Session TSF profile first (when en-US is installed), then per-hwnd HKL request.
    let _ = tsf::activate_english_via_tsf();
    post_layout_change(hwnd, hkl);
    std::thread::sleep(std::time::Duration::from_millis(40));
    // Verify against target thread HKL — not our process.
    let tid = unsafe { GetWindowThreadProcessId(hwnd, None) };
    let now = unsafe { GetKeyboardLayout(tid) };
    let lang = (now.0 as usize & 0xFFFF) as u16;
    if lang == LANG_EN_US {
        return Ok(());
    }
    // One more push
    post_layout_change(hwnd, hkl);
    Ok(())
}

pub fn activate_chinese_ime(hwnd: HWND) -> Result<(), String> {
    // Core prefers Microsoft Pinyin; WeChat is not an auto-switch target.
    match tsf::activate_preferred_chinese(false) {
        Ok(tip) => {
            tracing::info!("TSF activated {:?}", tip.backend);
            let _ = hwnd; // layout change is session/process scoped via TSF flags
            return Ok(());
        }
        Err(e) => tracing::warn!("TSF chinese activate failed: {e}; fallback HKL"),
    }

    let mut hkls = vec![HKL::default(); 16];
    let n = unsafe { GetKeyboardLayoutList(Some(&mut hkls)) };
    if n == 0 {
        return Err("GetKeyboardLayoutList empty".into());
    }
    hkls.truncate(n as usize);

    let mut zh = None;
    for hkl in hkls {
        let lang = (hkl.0 as usize & 0xFFFF) as u16;
        if lang == LANG_ZH_CN || (lang & 0xFF) == 0x04 {
            zh = Some(hkl);
            break;
        }
    }

    let hkl = zh
        .or_else(|| load_hkl("00000804").ok())
        .ok_or_else(|| "no Chinese layout found".to_string())?;
    post_layout_change(hwnd, hkl);
    Ok(())
}

pub fn classify_installed_imes() -> Vec<(isize, u16, String, ActiveIme)> {
    let mut out = Vec::new();

    // TSF tips first — this is the real inventory on Win10/11.
    for tip in tsf::enum_zh_profiles() {
        out.push((0, tip.langid, format!("TIP {:?}", tip.clsid), tip.backend));
    }

    use windows::Win32::UI::Input::Ime::ImmGetDescriptionW;
    let mut hkls = vec![HKL::default(); 32];
    let n = unsafe { GetKeyboardLayoutList(Some(&mut hkls)) };
    if n == 0 {
        return out;
    }
    hkls.truncate(n as usize);

    for hkl in hkls {
        let lang = (hkl.0 as usize & 0xFFFF) as u16;
        let mut buf = [0u16; 256];
        let len = unsafe { ImmGetDescriptionW(hkl, Some(&mut buf)) };
        let name = if len > 0 {
            String::from_utf16_lossy(&buf[..len as usize])
        } else {
            String::new()
        };
        let backend = if lang == LANG_EN_US {
            ActiveIme::LayoutEnglish
        } else {
            // Don't double-guess tip from empty Imm description.
            ActiveIme::OtherChinese
        };
        out.push((hkl.0 as isize, lang, format!("HKL {name}"), backend));
    }
    out
}
