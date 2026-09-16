//! Phase-1 diagnostics: observe Shift after terminal toggles. No second toggle.
//! LL hook is lazy: arm for terminal manual-override, then auto-unhook when idle.

use parking_lot::Mutex;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PeekMessageW, PostThreadMessageW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT,
    LLKHF_INJECTED, LLKHF_LOWER_IL_INJECTED, PM_REMOVE, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP,
    WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::ime::IMEF_EXTRA;

static STARTED: AtomicBool = AtomicBool::new(false);
static PINNED: AtomicBool = AtomicBool::new(false);
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);
/// Bumps invalidate delayed idle-release timers when we re-arm or clear early.
static RELEASE_GEN: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct ShiftPulse {
    pub ms_after_arm: u64,
    pub up: bool,
    pub vk: u16,
    pub scan: u16,
    pub flags: u32,
    pub extra: usize,
    pub source: &'static str,
}

#[derive(Debug, Clone)]
pub struct ShiftWatchReport {
    pub apply_id: u64,
    pub window_ms: u64,
    pub pulses: Vec<ShiftPulse>,
}

impl ShiftWatchReport {
    pub fn summarize(&self) -> String {
        if self.pulses.is_empty() {
            return format!(
                "apply={}: no VK_SHIFT in {}ms after arm",
                self.apply_id, self.window_ms
            );
        }
        let mut parts = Vec::with_capacity(self.pulses.len());
        for p in &self.pulses {
            parts.push(format!(
                "{}@{}ms src={} vk={} scan={:#x} flags={:#x} extra={:#x}",
                if p.up { "up" } else { "dn" },
                p.ms_after_arm,
                p.source,
                p.vk,
                p.scan,
                p.flags,
                p.extra
            ));
        }
        let ours = self
            .pulses
            .iter()
            .filter(|p| p.source == "imeflow")
            .count();
        let injected = self
            .pulses
            .iter()
            .filter(|p| p.source == "injected")
            .count();
        let physical = self
            .pulses
            .iter()
            .filter(|p| p.source == "physical")
            .count();
        format!(
            "apply={} window={}ms counts={{imeflow:{},injected:{},physical:{}}} detail=[{}]",
            self.apply_id,
            self.window_ms,
            ours,
            injected,
            physical,
            parts.join("; ")
        )
    }

    /// True when a non-ImeFlow Shift appeared inside the watch window.
    pub fn foreign_shift(&self) -> bool {
        self.pulses.iter().any(|p| p.source != "imeflow")
    }
}

struct ArmState {
    apply_id: u64,
    started: Instant,
    window: Duration,
    pulses: Vec<ShiftPulse>,
}

const USER_SHIFT_TAP_MAX: Duration = Duration::from_millis(700);

#[derive(Debug, Clone, Copy)]
struct ShiftTapCandidate {
    started: Instant,
    dirty: bool,
}

#[derive(Default)]
struct Shared {
    arm: Option<ArmState>,
    /// After terminal→英：只接受一次“干净的物理 Shift tap”，用于把 ime_layer 标回「中」。
    override_until: Option<Instant>,
    user_chinese: bool,
    shift_tap: Option<ShiftTapCandidate>,
    /// Track physical non-Shift keys while the lazy override hook is active.
    /// This lets us reject Shift+A / Shift+| / Ctrl+Shift as IME toggles.
    non_shift_down: HashSet<u32>,
}

static SHARED: once_cell::sync::Lazy<Arc<Mutex<Shared>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(Shared::default())));

pub fn classify_shift_source(extra: usize, flags: u32) -> &'static str {
    if extra == IMEF_EXTRA {
        "imeflow"
    } else if flags & (LLKHF_INJECTED.0 | LLKHF_LOWER_IL_INJECTED.0) != 0 {
        "injected"
    } else {
        "physical"
    }
}

/// Keep the LL hook for the whole process (`--trace-keys`).
pub fn pin_for_session() {
    PINNED.store(true, Ordering::SeqCst);
}

/// Install WH_KEYBOARD_LL on a dedicated thread (must pump messages).
pub fn start() -> Result<(), String> {
    if STARTED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
    std::thread::Builder::new()
        .name("imeflow-key-trace".into())
        .spawn(move || {
            match install_hook() {
                Ok(hook) => {
                    let _ = tx.send(Ok(()));
                    run_hook_loop(hook);
                }
                Err(e) => {
                    STARTED.store(false, Ordering::SeqCst);
                    let _ = tx.send(Err(e));
                }
            }
        })
        .map_err(|e| {
            STARTED.store(false, Ordering::SeqCst);
            format!("key_trace thread: {e}")
        })?;
    rx.recv()
        .map_err(|e| {
            STARTED.store(false, Ordering::SeqCst);
            format!("key_trace start: {e}")
        })?
}

fn install_hook() -> Result<HHOOK, String> {
    let module = unsafe { GetModuleHandleW(None) }.map_err(|e| format!("GetModuleHandleW: {e}"))?;
    let hook = unsafe {
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(low_level_proc), module, 0)
    }
    .map_err(|e| format!("SetWindowsHookExW WH_KEYBOARD_LL: {e}"))?;
    HOOK_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
    tracing::info!("key_trace: WH_KEYBOARD_LL armed on dedicated thread");
    Ok(hook)
}

fn run_hook_loop(hook: HHOOK) {
    let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
    loop {
        let ok = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if ok.0 == 0 || ok.0 == -1 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    unsafe {
        let _ = UnhookWindowsHookEx(hook);
    }
    HOOK_THREAD.store(0, Ordering::SeqCst);
    STARTED.store(false, Ordering::SeqCst);
    tracing::info!("key_trace: WH_KEYBOARD_LL released");
}

pub fn stop() {
    let tid = HOOK_THREAD.swap(0, Ordering::SeqCst);
    if tid != 0 {
        unsafe {
            let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
}

fn hook_needed(shared: &Shared) -> bool {
    if shared.arm.is_some() {
        return true;
    }
    match shared.override_until {
        Some(until) if Instant::now() <= until => true,
        _ => false,
    }
}

fn expire_override(shared: &mut Shared) {
    if let Some(until) = shared.override_until {
        if Instant::now() > until {
            shared.override_until = None;
            shared.shift_tap = None;
            shared.non_shift_down.clear();
        }
    }
}

/// Drop the LL hook when nothing is listening. Safe to call from the hook callback
/// (only posts WM_QUIT; Unhook runs after the callback returns).
fn release_if_idle() {
    if PINNED.load(Ordering::SeqCst) {
        return;
    }
    let needed = {
        let mut shared = SHARED.lock();
        expire_override(&mut shared);
        hook_needed(&shared)
    };
    if !needed {
        stop();
    }
}

fn schedule_release_after(delay: Duration) {
    let gen = RELEASE_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::Builder::new()
        .name("imeflow-hook-idle".into())
        .spawn(move || {
            std::thread::sleep(delay);
            if RELEASE_GEN.load(Ordering::SeqCst) != gen {
                return;
            }
            release_if_idle();
        })
        .ok();
}

/// Begin collecting VK_SHIFT for `window` after the next terminal Shift pulse.
pub fn arm_shift_window(apply_id: u64, window: Duration) {
    let mut shared = SHARED.lock();
    shared.arm = Some(ArmState {
        apply_id,
        started: Instant::now(),
        window,
        pulses: Vec::with_capacity(16),
    });
}

/// Pump this thread's messages while waiting, then return the watch report.
/// Call from the watcher thread so WinEvents keep flowing; LL events live on the hook thread.
pub fn finish_shift_window() -> Option<ShiftWatchReport> {
    let (apply_id, started, window) = {
        let shared = SHARED.lock();
        let arm = shared.arm.as_ref()?;
        (arm.apply_id, arm.started, arm.window)
    };

    // Keep the focus/message thread responsive while the hook thread records.
    let deadline = started + window;
    while Instant::now() < deadline {
        pump_once();
        std::thread::sleep(Duration::from_millis(10));
    }
    // One last drain.
    pump_once();

    let mut shared = SHARED.lock();
    let arm = shared.arm.take()?;
    drop(shared);
    release_if_idle();
    Some(ShiftWatchReport {
        apply_id,
        window_ms: window.as_millis() as u64,
        pulses: arm.pulses,
    })
}

pub fn cancel_shift_window() {
    SHARED.lock().arm = None;
    release_if_idle();
}

/// terminal→英 之后武装：用户非 ImeFlow 的 Shift → take_user_chinese_override。
/// 懒启动 LL hook（不必开 --trace-keys）；窗口结束后自动卸钩。
pub fn arm_user_override(window: Duration) {
    if let Err(e) = start() {
        tracing::warn!(%e, "user-override arm skipped (hook start failed)");
        return;
    }
    {
        let mut shared = SHARED.lock();
        shared.override_until = Some(Instant::now() + window);
        shared.user_chinese = false;
        shared.shift_tap = None;
        shared.non_shift_down.clear();
    }
    // +200ms slack so the last key in the window is still observed.
    schedule_release_after(window + Duration::from_millis(200));
    tracing::info!(
        window_ms = window.as_millis() as u64,
        "key_trace: user-override armed (clean physical Shift tap → memory 中)"
    );
}

pub fn clear_user_override() {
    RELEASE_GEN.fetch_add(1, Ordering::SeqCst);
    {
        let mut shared = SHARED.lock();
        shared.override_until = None;
        shared.user_chinese = false;
        shared.shift_tap = None;
        shared.non_shift_down.clear();
    }
    release_if_idle();
}

/// Consume a completed clean physical Shift tap. The caller should invoke this before
/// clearing per-root state, so “temporary Chinese then immediately switch app” is not lost.
pub fn take_user_chinese_override() -> bool {
    let hit = {
        let mut shared = SHARED.lock();
        expire_override(&mut shared);
        let hit = shared.user_chinese;
        if hit {
            shared.user_chinese = false;
            shared.override_until = None;
            shared.shift_tap = None;
            shared.non_shift_down.clear();
        }
        hit
    };
    if hit {
        RELEASE_GEN.fetch_add(1, Ordering::SeqCst);
        release_if_idle();
    }
    hit
}

fn pump_once() {
    let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
    while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() } {
        if msg.message == WM_QUIT {
            unsafe {
                let _ = PostThreadMessageW(
                    GetCurrentThreadId(),
                    WM_QUIT,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn low_level_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let msg = wparam.0 as u32;
        if matches!(msg, WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP) {
            let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            note_key(kb, matches!(msg, WM_KEYUP | WM_SYSKEYUP));
        }
    }
    CallNextHookEx(HHOOK::default(), code, wparam, lparam)
}

fn is_shift_vk(vk: u32) -> bool {
    matches!(vk, 0x10 | 0xA0 | 0xA1)
}

fn note_override_key(shared: &mut Shared, vk: u32, up: bool, source: &'static str, now: Instant) {
    let active = shared
        .override_until
        .map(|until| now <= until)
        .unwrap_or(false);
    if !active {
        if shared.override_until.is_some() {
            shared.override_until = None;
        }
        shared.shift_tap = None;
        shared.non_shift_down.clear();
        return;
    }

    if is_shift_vk(vk) {
        // Only a real physical Shift is treated as a manual IME override. Other injected
        // Shift pulses are diagnostic evidence, not proof of user intent.
        if source != "physical" {
            if !up {
                shared.shift_tap = None;
            }
            return;
        }
        if !up {
            // Ignore modifier auto-repeat; preserve the first down time.
            if shared.shift_tap.is_none() {
                let dirty = !shared.non_shift_down.is_empty();
                shared.shift_tap = Some(ShiftTapCandidate {
                    started: now,
                    dirty,
                });
            }
            return;
        }

        let Some(candidate) = shared.shift_tap.take() else { return };
        let held = now.saturating_duration_since(candidate.started);
        if !candidate.dirty && held <= USER_SHIFT_TAP_MAX {
            // The arm starts immediately after an automatic terminal→英 pulse. Clean manual
            // taps toggle from that baseline, so parity matters: one tap = 中, two taps = 英.
            shared.user_chinese = !shared.user_chinese;
            tracing::info!(held_ms = held.as_millis() as u64, manual_chinese = shared.user_chinese,
                "key_trace: clean physical Shift tap → toggle manual override parity");
        } else {
            tracing::debug!(dirty = candidate.dirty, held_ms = held.as_millis() as u64,
                "key_trace: Shift chord/hold ignored for IME override");
        }
        return;
    }

    // Any non-Shift key used while Shift is held turns the candidate into a normal chord.
    if !up {
        if source == "physical" {
            shared.non_shift_down.insert(vk);
        }
        if let Some(candidate) = shared.shift_tap.as_mut() {
            candidate.dirty = true;
        }
    } else if source == "physical" {
        shared.non_shift_down.remove(&vk);
    }
}

fn note_key(kb: &KBDLLHOOKSTRUCT, up: bool) {
    let flags = kb.flags.0;
    let extra = kb.dwExtraInfo;
    let source = classify_shift_source(extra, flags);
    let now = Instant::now();
    let vk = kb.vkCode;

    let idle = {
        let mut shared = SHARED.lock();
        note_override_key(&mut shared, vk, up, source, now);

        if is_shift_vk(vk) {
            if let Some(arm) = shared.arm.as_mut() {
                let elapsed = arm.started.elapsed();
                if elapsed <= arm.window {
                    arm.pulses.push(ShiftPulse {
                        ms_after_arm: elapsed.as_millis() as u64,
                        up,
                        vk: kb.vkCode as u16,
                        scan: kb.scanCode as u16,
                        flags,
                        extra,
                        source,
                    });
                }
            }
        }
        expire_override(&mut shared);
        !hook_needed(&shared)
    };
    // Post quit only; Unhook runs after this callback returns.
    if idle {
        release_if_idle();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_prefers_imeflow_tag() {
        assert_eq!(classify_shift_source(IMEF_EXTRA, LLKHF_INJECTED.0), "imeflow");
        assert_eq!(classify_shift_source(0, LLKHF_INJECTED.0), "injected");
        assert_eq!(classify_shift_source(0, 0), "physical");
    }

    #[test]
    fn report_detects_foreign_shift() {
        let mut report = ShiftWatchReport {
            apply_id: 1,
            window_ms: 500,
            pulses: vec![],
        };
        assert!(!report.foreign_shift());
        report.pulses.push(ShiftPulse {
            ms_after_arm: 10,
            up: false,
            vk: 0xA0,
            scan: 0x2A,
            flags: 0,
            extra: IMEF_EXTRA,
            source: "imeflow",
        });
        assert!(!report.foreign_shift());
        report.pulses.push(ShiftPulse {
            ms_after_arm: 20,
            up: false,
            vk: 0xA0,
            scan: 0x2A,
            flags: 0,
            extra: 0,
            source: "physical",
        });
        assert!(report.foreign_shift());
    }

    #[test]
    fn clean_shift_tap_toggles_override_parity() {
        let mut s = Shared::default();
        let t = Instant::now();
        s.override_until = Some(t + Duration::from_secs(5));
        note_override_key(&mut s, 0xA0, false, "physical", t);
        note_override_key(&mut s, 0xA0, true, "physical", t + Duration::from_millis(120));
        assert!(s.user_chinese);
        note_override_key(&mut s, 0xA0, false, "physical", t + Duration::from_millis(200));
        note_override_key(&mut s, 0xA0, true, "physical", t + Duration::from_millis(280));
        assert!(!s.user_chinese);
    }

    #[test]
    fn shift_chord_is_ignored() {
        let mut s = Shared::default();
        let t = Instant::now();
        s.override_until = Some(t + Duration::from_secs(5));
        note_override_key(&mut s, 0x41, false, "physical", t);
        note_override_key(&mut s, 0xA0, false, "physical", t + Duration::from_millis(30));
        note_override_key(&mut s, 0xA0, true, "physical", t + Duration::from_millis(80));
        assert!(!s.user_chinese);
    }

    #[test]
    fn ctrl_shift_is_ignored() {
        let mut s = Shared::default();
        let t = Instant::now();
        s.override_until = Some(t + Duration::from_secs(5));
        note_override_key(&mut s, 0xA0, false, "physical", t);
        note_override_key(&mut s, 0x11, true, "physical", t + Duration::from_millis(20));
        note_override_key(&mut s, 0x11, false, "physical", t + Duration::from_millis(40));
        note_override_key(&mut s, 0xA0, true, "physical", t + Duration::from_millis(100));
        assert!(!s.user_chinese);
    }

    #[test]
    fn injected_shift_is_ignored() {
        let mut s = Shared::default();
        let t = Instant::now();
        s.override_until = Some(t + Duration::from_secs(5));
        note_override_key(&mut s, 0xA0, false, "injected", t);
        note_override_key(&mut s, 0xA0, true, "injected", t + Duration::from_millis(100));
        assert!(!s.user_chinese);
    }

    #[test]
    fn long_hold_is_ignored() {
        let mut s = Shared::default();
        let t = Instant::now();
        s.override_until = Some(t + Duration::from_secs(5));
        note_override_key(&mut s, 0xA0, false, "physical", t);
        note_override_key(
            &mut s,
            0xA0,
            true,
            "physical",
            t + USER_SHIFT_TAP_MAX + Duration::from_millis(1),
        );
        assert!(!s.user_chinese);
    }

    #[test]
    fn hook_needed_respects_override_window() {
        let mut s = Shared::default();
        assert!(!hook_needed(&s));
        let t = Instant::now();
        s.override_until = Some(t + Duration::from_secs(1));
        assert!(hook_needed(&s));
        s.override_until = Some(t - Duration::from_millis(1));
        expire_override(&mut s);
        assert!(!hook_needed(&s));
    }
}
