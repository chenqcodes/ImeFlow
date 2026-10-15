//! Shared presentation for the native settings windows.
use std::sync::OnceLock;
use windows::{
    core::w,
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Gdi::*,
        UI::{
            Controls::{DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS, ODS_SELECTED},
            WindowsAndMessaging::*,
        },
    },
};

fn color(r: u32, g: u32, b: u32) -> COLORREF {
    COLORREF(r | g << 8 | b << 16)
}
pub fn background() -> HBRUSH {
    static BRUSH: OnceLock<usize> = OnceLock::new();
    HBRUSH(
        *BRUSH.get_or_init(|| unsafe { CreateSolidBrush(color(246, 248, 252)).0 as usize })
            as *mut _,
    )
}
pub fn white() -> HBRUSH {
    unsafe { HBRUSH(GetStockObject(WHITE_BRUSH).0) }
}
pub struct Heading(pub HFONT);
impl Heading {
    pub fn new(dpi: i32) -> Self {
        unsafe {
            Self(CreateFontW(
                -20 * dpi / 96,
                0,
                0,
                0,
                600,
                0,
                0,
                0,
                DEFAULT_CHARSET.0 as u32,
                OUT_DEFAULT_PRECIS.0 as u32,
                CLIP_DEFAULT_PRECIS.0 as u32,
                CLEARTYPE_QUALITY.0 as u32,
                DEFAULT_PITCH.0 as u32,
                w!("Microsoft YaHei UI"),
            ))
        }
    }
}
impl Drop for Heading {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.0);
        }
    }
}
/// Native buttons retain their labels and keyboard navigation; only painting is shared.
pub unsafe fn paint(msg: u32, wp: WPARAM, lp: LPARAM, primary: &[usize]) -> Option<LRESULT> {
    match msg {
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let dc = HDC(wp.0 as *mut _);
            let mut class = [0u16; 32];
            GetClassNameW(HWND(lp.0 as *mut _), &mut class);
            let is_static = String::from_utf16_lossy(&class).starts_with("Static");
            let bg = if is_static {
                color(246, 248, 252)
            } else {
                color(255, 255, 255)
            };
            SetBkColor(dc, bg);
            SetTextColor(dc, color(40, 50, 68));
            Some(LRESULT(
                if is_static { background().0 } else { white().0 } as isize
            ))
        }
        WM_DRAWITEM => {
            let item = &*(lp.0 as *const DRAWITEMSTRUCT);
            if item.CtlType.0 == 2 {
                let r = item.rcItem;
                let selected = item.itemState.0 & ODS_SELECTED.0 != 0;
                let brush = CreateSolidBrush(if selected {
                    color(229, 237, 253)
                } else {
                    color(255, 255, 255)
                });
                FillRect(item.hDC, &r, brush);
                let _ = DeleteObject(brush);
                if item.itemID == u32::MAX {
                    return Some(LRESULT(1));
                }
                let len = SendMessageW(
                    item.hwndItem,
                    LB_GETTEXTLEN,
                    WPARAM(item.itemID as usize),
                    LPARAM(0),
                )
                .0;
                if len < 0 {
                    return Some(LRESULT(1));
                }
                let mut text = vec![0u16; len as usize + 1];
                SendMessageW(
                    item.hwndItem,
                    LB_GETTEXT,
                    WPARAM(item.itemID as usize),
                    LPARAM(text.as_mut_ptr() as isize),
                );
                let label = String::from_utf16_lossy(&text[..len as usize]);
                let (scope, mode) = label.split_once(" → ").unwrap_or((&label, ""));
                let font = SendMessageW(item.hwndItem, WM_GETFONT, WPARAM(0), LPARAM(0));
                let old = SelectObject(item.hDC, HGDIOBJ(font.0 as *mut _));
                SetBkMode(item.hDC, TRANSPARENT);
                let pad = (r.bottom - r.top) / 4;
                for (line, value) in [scope, mode].iter().enumerate() {
                    let mut rect = r;
                    rect.left += pad;
                    rect.right -= pad;
                    if line == 0 {
                        rect.bottom = (r.top + r.bottom) / 2;
                        rect.top += 2;
                    } else {
                        rect.top = (r.top + r.bottom) / 2;
                        rect.bottom -= 2;
                    }
                    SetTextColor(
                        item.hDC,
                        if line == 0 {
                            color(40, 50, 68)
                        } else {
                            color(81, 105, 142)
                        },
                    );
                    let mut value: Vec<u16> = value.encode_utf16().collect();
                    DrawTextW(
                        item.hDC,
                        &mut value,
                        &mut rect,
                        DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
                    );
                }
                SelectObject(item.hDC, old);
                if item.itemState.0 & ODS_FOCUS.0 != 0 {
                    let _ = DrawFocusRect(item.hDC, &r);
                }
                return Some(LRESULT(1));
            }
            if item.CtlType.0 != 4 {
                return None;
            }
            let disabled = (item.itemState.0 & ODS_DISABLED.0) != 0;
            let selected = (item.itemState.0 & ODS_SELECTED.0) != 0;
            let accent = primary.contains(&(item.CtlID as usize));
            let fill = if disabled {
                color(231, 235, 241)
            } else if accent {
                if selected {
                    color(35, 75, 170)
                } else {
                    color(48, 96, 205)
                }
            } else if selected {
                color(224, 231, 244)
            } else {
                color(255, 255, 255)
            };
            let brush = CreateSolidBrush(fill);
            let pen = CreatePen(
                PS_SOLID,
                1,
                if accent { fill } else { color(213, 222, 235) },
            );
            let old_brush = SelectObject(item.hDC, brush);
            let old_pen = SelectObject(item.hDC, pen);
            let r = item.rcItem;
            let _ = RoundRect(item.hDC, r.left, r.top, r.right, r.bottom, 10, 10);
            SelectObject(item.hDC, old_brush);
            SelectObject(item.hDC, old_pen);
            let _ = DeleteObject(brush);
            let _ = DeleteObject(pen);
            let font = SendMessageW(item.hwndItem, WM_GETFONT, WPARAM(0), LPARAM(0));
            let old_font = SelectObject(item.hDC, HGDIOBJ(font.0 as *mut _));
            SetBkMode(item.hDC, TRANSPARENT);
            SetTextColor(
                item.hDC,
                if disabled {
                    color(142, 151, 164)
                } else if accent {
                    color(255, 255, 255)
                } else {
                    color(46, 58, 79)
                },
            );
            let mut label = [0u16; 256];
            let len = GetWindowTextW(item.hwndItem, &mut label) as usize;
            let mut text_rect = r;
            DrawTextW(
                item.hDC,
                &mut label[..len],
                &mut text_rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            SelectObject(item.hDC, old_font);
            if (item.itemState.0 & ODS_FOCUS.0) != 0 {
                let mut focus = r;
                focus.left += 4;
                focus.top += 4;
                focus.right -= 4;
                focus.bottom -= 4;
                let _ = DrawFocusRect(item.hDC, &focus);
            }
            Some(LRESULT(1))
        }
        _ => None,
    }
}
