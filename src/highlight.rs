//! Four narrow, non-activating windows outline a captured UIA rectangle.
use windows::{
    core::w,
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{GetSysColorBrush, COLOR_HIGHLIGHT},
        UI::WindowsAndMessaging::*,
    },
};

pub struct Highlight {
    windows: Vec<HWND>,
    owner: HWND,
    owner_rect: RECT,
}
fn edges(rect: [i32; 4]) -> Option<[[i32; 4]; 4]> {
    let [left, top, right, bottom] = rect;
    let width = right.checked_sub(left)?;
    let height = bottom.checked_sub(top)?;
    if width < 2 || height < 2 {
        return None;
    }
    let stroke = 3.min(width).min(height);
    Some([
        [left, top, width, stroke],
        [left, bottom - stroke, width, stroke],
        [left, top, stroke, height],
        [right - stroke, top, stroke, height],
    ])
}
impl Highlight {
    pub fn show(owner: HWND, rect: [i32; 4]) -> Result<Self, String> {
        Self::create(owner, rect, true)
    }
    fn create(owner: HWND, rect: [i32; 4], visible: bool) -> Result<Self, String> {
        let boxes = edges(rect).ok_or("目标没有有效的屏幕位置")?;
        unsafe {
            let mut owner_rect = RECT::default();
            GetWindowRect(owner, &mut owner_rect).map_err(|e| e.to_string())?;
            let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
                .map_err(|e| e.to_string())?;
            let class = w!("ImeFlowSelectionBorder");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wnd_proc),
                hInstance: instance.into(),
                lpszClassName: class,
                hbrBackground: GetSysColorBrush(COLOR_HIGHLIGHT),
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);
            let mut overlay = Self {
                windows: Vec::new(),
                owner,
                owner_rect,
            };
            for [x, y, width, height] in boxes {
                let hwnd = CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
                    class,
                    w!(""),
                    WS_POPUP,
                    x,
                    y,
                    width,
                    height,
                    None,
                    None,
                    instance,
                    None,
                )
                .map_err(|e| e.to_string())?;
                overlay.windows.push(hwnd);
                if visible {
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                }
            }
            Ok(overlay)
        }
    }
    /// Hide instead of drawing a stale rectangle when the app/window moves or is left.
    pub fn still_positioned(&self) -> bool {
        unsafe {
            if !IsWindow(self.owner).as_bool() || IsIconic(self.owner).as_bool() {
                return false;
            }
            let fg = crate::process::root_hwnd(crate::process::foreground_hwnd());
            if fg != self.owner && crate::process::hwnd_process_id(fg) != std::process::id() {
                return false;
            }
            let mut rect = RECT::default();
            GetWindowRect(self.owner, &mut rect).is_ok() && rect == self.owner_rect
        }
    }
}
impl Drop for Highlight {
    fn drop(&mut self) {
        for hwnd in self.windows.drain(..) {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
    }
}
unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_borders_do_not_activate_and_are_destroyed() {
        unsafe {
            let owner = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("STATIC"),
                w!("ImeFlow border test"),
                WS_POPUP,
                0,
                0,
                100,
                100,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let foreground = GetForegroundWindow();
            let overlay = Highlight::show(owner, [10, 10, 90, 90]).unwrap();
            assert_eq!(GetForegroundWindow(), foreground);
            let windows = overlay.windows.clone();
            for hwnd in &windows {
                assert_eq!(
                    SendMessageW(*hwnd, WM_NCHITTEST, WPARAM(0), LPARAM(0)).0,
                    HTTRANSPARENT as isize
                );
                assert_eq!(
                    SendMessageW(*hwnd, WM_MOUSEACTIVATE, WPARAM(0), LPARAM(0)).0,
                    MA_NOACTIVATE as isize
                );
            }
            drop(overlay);
            assert!(windows.iter().all(|hwnd| !IsWindow(*hwnd).as_bool()));
            let _ = DestroyWindow(owner);
        }
    }
    #[test]
    fn border_keeps_input_interior_clear_and_supports_negative_monitor_coordinates() {
        let edges = edges([-200, 20, 100, 120]).unwrap();
        for [x, y, w, h] in edges {
            assert!(!(x <= 0 && 0 < x + w && y <= 70 && 70 < y + h));
        }
        assert!(super::edges([0, 0, 0, 20]).is_none());
        assert!(super::edges([i32::MIN, 0, i32::MAX, 20]).is_none());
    }
}
