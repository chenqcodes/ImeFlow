//! Foreground/focus hooks enqueue only. UIA and IME work runs outside the hook.

use std::cell::{Cell, RefCell};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, KillTimer, PostThreadMessageW, SetTimer, TranslateMessage,
    EVENT_OBJECT_FOCUS, EVENT_SYSTEM_FOREGROUND, MSG, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WM_QUIT, WM_TIMER,
};

const SETTLE_MS: u32 = 120;
type EventFn = Box<dyn FnMut(WatchEvent) + Send>;
static LOOP_THREAD: AtomicU32 = AtomicU32::new(0);

#[derive(Debug, Clone, Copy)]
pub enum WatchEvent {
    Foreground(HWND),
    Focus(HWND),
}

thread_local! {
    static CALLBACK: RefCell<Option<EventFn>> = RefCell::new(None);
    static PENDING: Cell<Option<WatchEvent>> = const { Cell::new(None) };
    static TIMER: Cell<usize> = const { Cell::new(0) };
    static GENERATION: Cell<u64> = const { Cell::new(0) };
}

fn cancel_timer() {
    let id = TIMER.with(|timer| timer.replace(0));
    if id != 0 {
        unsafe { let _ = KillTimer(None, id); }
    }
}

fn enqueue(ev: WatchEvent) {
    PENDING.with(|pending| pending.set(Some(ev)));
    cancel_timer();
    // No TimerProc: nested COM message pumping cannot invoke the business callback.
    let id = unsafe { SetTimer(None, 0, SETTLE_MS, None) };
    TIMER.with(|timer| timer.set(id));
    if id == 0 {
        tracing::error!("focus debounce timer could not be created; no IME action taken");
    }
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    event_thread: u32,
    event_time: u32,
) {
    if hwnd.0.is_null() {
        return;
    }
    let ev = match event {
        EVENT_SYSTEM_FOREGROUND => WatchEvent::Foreground(hwnd),
        EVENT_OBJECT_FOCUS => WatchEvent::Focus(hwnd),
        _ => return,
    };
    let seq = GENERATION.with(|g| {
        let next = g.get().wrapping_add(1);
        g.set(next);
        next
    });
    tracing::debug!(seq, event, event_thread, event_time, id_object, id_child,
        hwnd = ?hwnd, "focus event queued");
    enqueue(ev);
}

pub struct AppWatcher {
    hook_fg: HWINEVENTHOOK,
    hook_focus: HWINEVENTHOOK,
    // Hooks and thread-local callback must be dropped on their creating thread.
    _thread_bound: PhantomData<Rc<()>>,
}

impl AppWatcher {
    pub fn start<F>(on_event: F) -> windows::core::Result<Self>
    where
        F: FnMut(WatchEvent) + Send + 'static,
    {
        CALLBACK.with(|slot| *slot.borrow_mut() = Some(Box::new(on_event)));
        let hook_fg = unsafe {
            SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None,
                Some(win_event_proc), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS)
        };
        if hook_fg.0.is_null() {
            let error = windows::core::Error::from_win32();
            CALLBACK.with(|slot| { slot.borrow_mut().take(); });
            return Err(error);
        }
        let hook_focus = unsafe {
            SetWinEventHook(EVENT_OBJECT_FOCUS, EVENT_OBJECT_FOCUS, None,
                Some(win_event_proc), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS)
        };
        if hook_focus.0.is_null() {
            let error = windows::core::Error::from_win32();
            unsafe { let _ = UnhookWinEvent(hook_fg); }
            CALLBACK.with(|slot| { slot.borrow_mut().take(); });
            return Err(error);
        }
        LOOP_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
        tracing::info!("AppWatcher: queued FOREGROUND + FOCUS, settle={}ms", SETTLE_MS);
        Ok(Self { hook_fg, hook_focus, _thread_bound: PhantomData })
    }

    pub fn generation() -> u64 {
        GENERATION.with(Cell::get)
    }

    /// A bounded confirmation of an event, not an always-running poller.
    pub fn request_recheck() {
        let hwnd = crate::process::foreground_hwnd();
        if !hwnd.0.is_null() {
            enqueue(WatchEvent::Foreground(hwnd));
        }
    }

    pub fn run_message_loop() {
        unsafe {
            let mut msg = MSG::default();
            loop {
                let result = GetMessageW(&mut msg, None, 0, 0).0;
                if result <= 0 {
                    if result < 0 { tracing::error!("GetMessageW failed"); }
                    break;
                }
                if msg.message == WM_TIMER && msg.hwnd.0.is_null() {
                    let ours = TIMER.with(|timer| timer.get() != 0 && timer.get() == msg.wParam.0);
                    if ours {
                        cancel_timer();
                        let ev = PENDING.with(|pending| pending.take());
                        // Take ownership instead of retaining a RefCell borrow across COM calls.
                        let callback = CALLBACK.with(|slot| slot.borrow_mut().take());
                        if let Some(mut cb) = callback {
                            if let Some(ev) = ev { cb(ev); }
                            CALLBACK.with(|slot| *slot.borrow_mut() = Some(cb));
                        }
                        continue;
                    }
                }
                if crate::rule_panel::handle_message(&msg) { continue; }
                if crate::rule_manager::handle_message(&msg) { continue; }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    pub fn quit() {
        let tid = LOOP_THREAD.load(Ordering::SeqCst);
        if tid != 0 {
            unsafe { let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0)); }
        }
    }
}

impl Drop for AppWatcher {
    fn drop(&mut self) {
        unsafe {
            let _ = UnhookWinEvent(self.hook_fg);
            let _ = UnhookWinEvent(self.hook_focus);
        }
        cancel_timer();
        PENDING.with(|pending| pending.set(None));
        CALLBACK.with(|slot| { slot.borrow_mut().take(); });
        LOOP_THREAD.store(0, Ordering::SeqCst);
    }
}
