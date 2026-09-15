//! Status toast disabled for this build. API kept as no-ops so callers stay simple.
#![allow(dead_code)]
use crate::feedback::FeedbackBus;
use once_cell::sync::OnceCell;
use std::sync::atomic::{AtomicBool, Ordering};

static PINNED: AtomicBool = AtomicBool::new(false);
static EXTRA: OnceCell<parking_lot::Mutex<ExtraState>> = OnceCell::new();

#[derive(Default, Clone, PartialEq, Eq)]
pub struct ExtraState {
    pub foreground: String,
    pub control: String,
    pub snapshot: String,
    pub last_action: String,
    pub enabled: bool,
}

pub fn init(_bus: FeedbackBus) {
    let _ = EXTRA.set(parking_lot::Mutex::new(ExtraState::default()));
}

pub fn set_extra(f: impl FnOnce(&mut ExtraState)) {
    if let Some(extra) = EXTRA.get() {
        f(&mut extra.lock());
    }
}

pub fn request_refresh() {}
pub fn is_pinned() -> bool {
    PINNED.load(Ordering::SeqCst)
}
pub fn toggle() {
    let on = !PINNED.load(Ordering::SeqCst);
    PINNED.store(on, Ordering::SeqCst);
    tracing::info!(on, "status toast disabled in this build");
}
pub fn hide() {}
pub fn shutdown() {
    PINNED.store(false, Ordering::SeqCst);
}
pub fn notify_switch(_confirmed: bool) {}

#[cfg(test)]
mod tests {
    use crate::feedback::{StatusKind, StatusView};
    use crate::mode::TargetMode;

    fn status_text(view: &StatusView, enabled: bool) -> (String, String) {
        if !enabled {
            return ("ImeFlow".into(), "已暂停".into());
        }
        let app = if view.process.is_empty() {
            "待命".into()
        } else {
            view.process.clone()
        };
        let mode = match view.kind {
            StatusKind::Confirmed => match view.target {
                Some(TargetMode::NativeChinese) => "中",
                Some(TargetMode::ImeEnglish) => "英",
                Some(TargetMode::LayoutEnglish) => "ENG",
                _ => "—",
            },
            StatusKind::Requested => "待确认",
            _ => "—",
        };
        (app, mode.into())
    }

    #[test]
    fn count_fits_small_badge() {
        fn compact_count(n: u64) -> String {
            if n > 999 {
                "999+".into()
            } else {
                n.to_string()
            }
        }
        assert_eq!(compact_count(999), "999");
        assert_eq!(compact_count(1000), "999+");
    }

    #[test]
    fn requested_target_is_not_displayed_as_confirmed_mode() {
        let mut view = StatusView {
            process: "Edge · 输入框".into(),
            target: Some(TargetMode::NativeChinese),
            kind: StatusKind::Requested,
        };
        assert_eq!(status_text(&view, true).1, "待确认");
        view.kind = StatusKind::Confirmed;
        assert_eq!(status_text(&view, true).1, "中");
        view.target = Some(TargetMode::LayoutEnglish);
        assert_eq!(status_text(&view, true).1, "ENG");
        assert_eq!(status_text(&view, false).1, "已暂停");
    }
}
