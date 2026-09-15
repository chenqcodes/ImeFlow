use std::path::Path;
use std::time::{Duration, Instant};
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, BOOL, HANDLE, HWND, LPARAM, MAX_PATH};
use windows::Win32::System::ProcessStatus::K32GetModuleFileNameExW;
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetForegroundWindow, GetWindow, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, SetForegroundWindow, ShowWindow, GA_ROOT, GW_OWNER, SW_RESTORE,
};

pub fn foreground_hwnd() -> HWND {
    unsafe { GetForegroundWindow() }
}

pub fn hwnd_process_id(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    pid
}

pub fn hwnd_thread_id(hwnd: HWND) -> u32 {
    unsafe { GetWindowThreadProcessId(hwnd, None) }
}

pub fn exe_path_from_pid(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; MAX_PATH as usize];
        let len = K32GetModuleFileNameExW(HANDLE(handle.0), None, &mut buf);
        let _ = CloseHandle(handle);
        if len == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

pub fn exe_name_from_path(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}

pub fn hwnd_exe_name(hwnd: HWND) -> Option<String> {
    let pid = hwnd_process_id(hwnd);
    if pid == 0 {
        return None;
    }
    let path = exe_path_from_pid(pid)?;
    Some(exe_name_from_path(&path))
}

/// Climb to the top-level root window (for focus child hwnd → process).
pub fn root_hwnd(hwnd: HWND) -> HWND {
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    if root.0.is_null() {
        hwnd
    } else {
        root
    }
}

/// Native focus identity captured without activating or attaching any thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputTarget {
    pub root: isize,
    pub focus: isize,
    pub thread: u32,
}

impl InputTarget {
    pub fn capture(hwnd: HWND) -> Result<Self, String> {
        use windows::Win32::UI::WindowsAndMessaging::{GetGUIThreadInfo, GUITHREADINFO};
        let fg = foreground_hwnd();
        let root = root_hwnd(hwnd);
        if fg.0.is_null() || root_hwnd(fg) != root {
            return Err("focus changed: target is no longer foreground".into());
        }
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        unsafe { GetGUIThreadInfo(0, &mut info).map_err(|e| format!("foreground focus: {e}"))?; }
        if info.hwndFocus.0.is_null() || root_hwnd(info.hwndFocus) != root {
            return Err("focus unavailable or belongs to another window".into());
        }
        if foreground_hwnd() != fg {
            return Err("foreground changed during focus capture".into());
        }
        Ok(Self {
            root: root.0 as isize,
            focus: info.hwndFocus.0 as isize,
            thread: hwnd_thread_id(info.hwndFocus),
        })
    }

    pub fn ensure_current(&self) -> Result<(), String> {
        let hwnd = HWND(self.root as *mut std::ffi::c_void);
        if Self::capture(hwnd)? == *self { Ok(()) }
        else { Err("focus changed during IME transaction; cancelled".into()) }
    }
}

/// Phase-2A: wait until native hwndFocus stops moving, then return that target.
/// Not a fixed "sleep 500ms then hope" — quiet is measured on focus identity.
pub fn wait_focus_quiet(
    root: HWND,
    quiet: Duration,
    budget: Duration,
) -> Result<InputTarget, String> {
    let started = Instant::now();
    let mut last = InputTarget::capture(root)?;
    let mut stable_since = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(20));
        let now = InputTarget::capture(root)?;
        if now.focus == last.focus && now.thread == last.thread && now.root == last.root {
            if stable_since.elapsed() >= quiet {
                tracing::info!(
                    focus = format!("{:x}", now.focus as usize),
                    quiet_ms = quiet.as_millis() as u64,
                    waited_ms = started.elapsed().as_millis() as u64,
                    "terminal focus quiet"
                );
                return Ok(now);
            }
        } else {
            tracing::debug!(
                from = format!("{:x}", last.focus as usize),
                to = format!("{:x}", now.focus as usize),
                "terminal focus still moving"
            );
            last = now;
            stable_since = Instant::now();
        }
        if started.elapsed() >= budget {
            tracing::warn!(
                focus = format!("{:x}", last.focus as usize),
                budget_ms = budget.as_millis() as u64,
                "terminal focus quiet budget exceeded; skip Shift"
            );
            return Err("terminal focus never quiet; skip Shift".into());
        }
    }
}

struct EnumFind {
    want: String,
    found: HWND,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let state = &mut *(lparam.0 as *mut EnumFind);
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }
        // Top-level only (no owner).
        let owner = GetWindow(hwnd, GW_OWNER).unwrap_or_default();
        if !owner.0.is_null() {
            return BOOL(1);
        }
        let Some(name) = hwnd_exe_name(hwnd) else {
            return BOOL(1);
        };
        if name.eq_ignore_ascii_case(&state.want) {
            state.found = hwnd;
            return BOOL(0); // stop
        }
    }
    BOOL(1)
}

/// First visible top-level window whose process exe matches (case-insensitive).
pub fn find_hwnd_by_exe(exe_name: &str) -> Option<HWND> {
    let mut state = EnumFind {
        want: exe_name.to_string(),
        found: HWND::default(),
    };
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut state as *mut _ as isize));
    }
    if state.found.0.is_null() {
        None
    } else {
        Some(state.found)
    }
}

pub fn focus_hwnd(hwnd: HWND) -> bool {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let fg = GetForegroundWindow();
        let fg_tid = GetWindowThreadProcessId(fg, None);
        let target_tid = GetWindowThreadProcessId(hwnd, None);
        let self_tid = GetCurrentThreadId();
        if fg_tid != 0 && fg_tid != self_tid {
            let _ = AttachThreadInput(self_tid, fg_tid, true);
        }
        if target_tid != 0 && target_tid != self_tid {
            let _ = AttachThreadInput(self_tid, target_tid, true);
        }
        let ok = SetForegroundWindow(hwnd).as_bool();
        if target_tid != 0 && target_tid != self_tid {
            let _ = AttachThreadInput(self_tid, target_tid, false);
        }
        if fg_tid != 0 && fg_tid != self_tid {
            let _ = AttachThreadInput(self_tid, fg_tid, false);
        }
        ok
    }
}

#[allow(dead_code)]
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[allow(dead_code)]
pub fn from_wide_buf(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

#[allow(dead_code)]
pub unsafe fn pwstr_to_string(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    p.to_string().unwrap_or_default()
}
