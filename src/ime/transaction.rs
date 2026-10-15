//! One input transaction retains the original native and UIA focus until its last write.
use crate::{
    process::InputTarget,
    uia::{self, ControlKind, FocusInfo},
};
use std::cell::Cell;
use windows::Win32::Foundation::HWND;

#[derive(Debug)]
pub struct ApplyError {
    pub message: String,
    /// Conservative: once a write may have happened, a second toggle is unsafe.
    pub may_have_changed: bool,
}
impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}

#[derive(Clone)]
pub(crate) struct FocusIdentity {
    process_id: u32,
    owner_root: isize,
    runtime_id: Vec<i32>,
    kind: ControlKind,
}
impl FocusIdentity {
    fn from_info(info: &FocusInfo) -> Result<Self, String> {
        if info.process_id == 0 || info.runtime_id.is_empty() {
            return Err("UIA focus identity unavailable; no IME write".into());
        }
        Ok(Self {
            process_id: info.process_id,
            owner_root: info.owner_root,
            runtime_id: info.runtime_id.clone(),
            kind: info.kind,
        })
    }
    fn matches(&self, other: &FocusInfo) -> bool {
        self.process_id == other.process_id
            && self.runtime_id == other.runtime_id
            && self.kind == other.kind
            && (self.owner_root == 0 || other.owner_root == self.owner_root)
    }
}

pub(crate) struct Transaction {
    input: InputTarget,
    identity: FocusIdentity,
    effect: Cell<bool>,
}
impl Transaction {
    pub fn capture(hwnd: HWND, focus: &FocusInfo) -> Result<Self, String> {
        let input = InputTarget::capture(hwnd)?;
        if focus.process_id != crate::process::hwnd_process_id(hwnd)
            && focus.owner_root != input.root
        {
            return Err("UIA focus belongs to another foreground root; no IME write".into());
        }
        let transaction = Self {
            input,
            identity: FocusIdentity::from_info(focus)?,
            effect: Cell::new(false),
        };
        transaction.ensure_current()?;
        Ok(transaction)
    }
    pub fn ensure_current(&self) -> Result<(), String> {
        self.input.ensure_current()?;
        let info = uia::inspect_focused().ok_or("UIA focus unavailable during IME transaction")?;
        if !self.identity.matches(&info) {
            return Err("logical input region changed during IME transaction; cancelled".into());
        }
        // UIA itself can block/pump COM messages. Native validation must be last.
        self.input.ensure_current()
    }
    pub fn before_write(&self) -> Result<(), String> {
        self.ensure_current()?;
        self.effect.set(true);
        Ok(())
    }
    pub fn note_effect(&self) {
        self.effect.set(true);
    }
    pub fn has_effect(&self) -> bool {
        self.effect.get()
    }
    pub fn error(&self, message: String) -> ApplyError {
        ApplyError {
            message,
            may_have_changed: self.effect.get(),
        }
    }
    pub fn focus_hwnd(&self) -> isize {
        self.input.focus
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn info(runtime: i32, kind: ControlKind) -> FocusInfo {
        FocusInfo {
            process_id: 10,
            owner_root: 42,
            runtime_id: vec![runtime],
            kind,
            ..Default::default()
        }
    }
    #[test]
    fn same_native_window_does_not_make_a_different_logical_input_safe() {
        let original = info(1, ControlKind::Terminal);
        let identity = FocusIdentity::from_info(&original).unwrap();
        assert!(identity.matches(&original));
        assert!(!identity.matches(&info(2, ControlKind::Edit)));
        assert!(!identity.matches(&info(1, ControlKind::Edit)));
        let mut other = original.clone();
        other.process_id = 11;
        assert!(!identity.matches(&other));
    }
    #[test]
    fn unknown_identity_cannot_authorize_a_write() {
        assert!(FocusIdentity::from_info(&FocusInfo::default()).is_err());
    }
    #[test]
    fn error_retry_safety_tracks_possible_side_effects() {
        let transaction = Transaction {
            input: InputTarget {
                root: 0,
                focus: 0,
                thread: 0,
            },
            identity: FocusIdentity::from_info(&info(1, ControlKind::Edit)).unwrap(),
            effect: Cell::new(false),
        };
        assert!(
            !transaction
                .error("cancelled before write".into())
                .may_have_changed
        );
        transaction.effect.set(true);
        assert!(
            transaction
                .error("unconfirmed after write".into())
                .may_have_changed
        );
    }
}
