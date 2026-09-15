mod asset_pixels;
mod assets;
mod debug_ui;
mod feedback;
mod focus_gate;
mod ime;
mod key_trace;
mod mode;
mod process;
mod rules;
mod tray;
mod ui_graphics;
mod uia;
mod watcher;

use feedback::{FeedbackBus, SwitchEvent};
use ime::{classify_installed_imes, ActiveIme, ImeController};
use mode::TargetMode;
use rules::{resolve_rules_path, RuleEngine};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing_subscriber::EnvFilter;
use watcher::{AppWatcher, WatchEvent};
use windows::Win32::Foundation::HWND;

/// Opt-in: after terminal Shift, type a tagged `b` so you can see 中 vs 英 by eye.
static TRACE_PROBE: AtomicBool = AtomicBool::new(false);
/// Opt-in: WH_KEYBOARD_LL Shift watch (`--trace-keys`).
static TRACE_KEYS: AtomicBool = AtomicBool::new(false);
/// 托盘重新勾选「自动切换」后：清掉暂停期间卡住的 gate / expected_mode。
static RESYNC_AFTER_ENABLE: AtomicBool = AtomicBool::new(false);

/// 托盘重新启用自动时调用（同线程消息循环）。
pub(crate) fn mark_resync_after_enable() {
    RESYNC_AFTER_ENABLE.store(true, Ordering::SeqCst);
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_ansi(false)
        .with_target(false)
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--check-assets") {
        // Validation-only entry: no WinEvent hooks, key injection, tray or statistics writes.
        if let Err(error) = assets::validate() {
            tracing::error!(%error, "asset validation failed");
            std::process::exit(1);
        }
        return;
    }
    if args.iter().any(|a| a == "--probe") {
        run_probe();
        return;
    }
    if args.iter().any(|a| a == "--probe-focus") {
        run_probe_focus();
        return;
    }
    if args.iter().any(|a| a == "--probe-shift") {
        run_probe_shift();
        return;
    }
    if args.iter().any(|a| a == "--verify-switch") {
        run_verify_switch();
        return;
    }
    if args.iter().any(|a| a == "--trace-probe") {
        TRACE_PROBE.store(true, Ordering::SeqCst);
        // 字母探针依赖 Shift 观测窗口
        TRACE_KEYS.store(true, Ordering::SeqCst);
    }
    if args.iter().any(|a| a == "--trace-keys") {
        TRACE_KEYS.store(true, Ordering::SeqCst);
    }

    if other_imeflow_running(std::process::id()) {
        tracing::error!("Another imeflow.exe is running. Stop it before testing this build.");
        std::process::exit(2);
    }
    tracing::info!("ImeFlow starting build=m1.1-assets1 pid={}", std::process::id());
    // UI assets and DPI are initialized before any input hooks or IME transactions.
    unsafe {
        use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    if let Err(error) = assets::get() {
        tracing::error!(%error, "assets unavailable; refusing to start with old or missing graphics");
        std::process::exit(1);
    }
    tracing::info!("prefer: WeChat IME primary, Microsoft Pinyin parallel");
    tracing::info!(
        trace_keys = TRACE_KEYS.load(Ordering::SeqCst),
        trace_probe = TRACE_PROBE.load(Ordering::SeqCst),
        "M1: key_trace off by default; use --trace-keys / --trace-probe"
    );
    log_installed_imes();

    if TRACE_KEYS.load(Ordering::SeqCst) {
        if let Err(e) = key_trace::start() {
            tracing::error!("key_trace start failed: {e}");
            std::process::exit(1);
        }
    }

    let rules_path = resolve_rules_path();
    let engine = match RuleEngine::load_from(&rules_path) {
        Ok(e) => {
            tracing::info!("loaded {} rules from {}", e.len(), rules_path.display());
            Arc::new(e)
        }
        Err(e) => {
            key_trace::stop();
            tracing::error!("invalid rules; app was not started: {e}");
            std::process::exit(1)
        }
    };

    let bus = FeedbackBus::default();
    key_trace::bind_feedback(bus.clone());
    debug_ui::init(bus.clone());
    if let Err(e) = tray::install(bus.clone(), true) {
        key_trace::stop();
        debug_ui::shutdown();
        tracing::error!("tray failed; app was not started: {e}");
        std::process::exit(1);
    }

    ctrlc_stub();

    let engine_hook = Arc::clone(&engine);
    let bus_hook = bus.clone();
    let last_key = Arc::new(parking_lot::Mutex::new(LastMatch::default()));
    let last_key_hook = Arc::clone(&last_key);

    match AppWatcher::start(move |ev| {
        on_watch(ev, &engine_hook, &bus_hook, &last_key_hook);
    }) {
        Ok(_w) => {
            let fg = process::foreground_hwnd();
            if !fg.0.is_null() {
                on_watch(
                    WatchEvent::Foreground(fg),
                    &engine,
                    &bus,
                    &last_key,
                );
            }
            tracing::info!("message loop running — tray: auto / status / autostart / logs / exit");
            AppWatcher::run_message_loop();
            debug_ui::shutdown();
            tray::remove();
            key_trace::stop();
        }
        Err(e) => {
            debug_ui::shutdown();
            tray::remove();
            key_trace::stop();
            tracing::error!("SetWinEventHook failed: {e}");
            std::process::exit(1);
        }
    }
}

struct LastMatch {
    root: isize,
    gate: focus_gate::FocusGate<String, TargetMode>,
    next_apply: u64,
    generation: u64,
    rechecks: u8,
    /// Last mode applied for the current foreground root (cleared on root change).
    expected_mode: Option<TargetMode>,
    /// Last 中/英 layer we intentionally wrote. Survives passthrough + root change
    /// (fallback only when live observation is Unknown).
    ime_layer: Option<TargetMode>,
    /// After terminal→英, briefly block same-exe 中 so Cursor→Codex focus hops don't 英中英.
    block_chinese_until: Option<(String, Instant)>,
    /// Cursor/xterm can briefly move focus through a helper Edit after terminal focus.
    cursor_terminal_seen_at: Option<Instant>,
    cursor_terminal_guard_until: Option<Instant>,
    /// Last successful IME write — used to suppress 中↔英 flicker on focus hops.
    last_apply_at: Option<Instant>,
}

impl Default for LastMatch {
    fn default() -> Self {
        Self {
            root: 0,
            gate: focus_gate::FocusGate::new(Duration::from_millis(100)),
            next_apply: 0,
            generation: 0,
            rechecks: 0,
            expected_mode: None,
            ime_layer: None,
            block_chinese_until: None,
            cursor_terminal_seen_at: None,
            cursor_terminal_guard_until: None,
            last_apply_at: None,
        }
    }
}

fn request_focus_confirmation(last: &parking_lot::Mutex<LastMatch>) {
    let schedule = {
        let mut state = last.lock();
        if state.rechecks >= 8 { false }
        else { state.rechecks += 1; true }
    };
    if schedule { AppWatcher::request_recheck(); }
    else { tracing::warn!("focus/state confirmation budget exhausted; wait for the next real focus event"); }
}

fn focus_key(root: HWND, info: &uia::FocusInfo) -> Option<String> {
    let root_pid = process::hwnd_process_id(root);
    // Electron/WinUI may expose a focused UIA element through another native root
    // inside the same process. Process identity is the authoritative cross-app guard;
    // owner_root is useful metadata but must not reject a same-process focus.
    let same_process = info.process_id != 0 && info.process_id == root_pid;
    let same_root = info.owner_root != 0 && info.owner_root == root.0 as isize;
    if (!same_process && !same_root) || info.runtime_id.is_empty() { return None; }
    Some(format!("{:x}:{}:{:?}", root.0 as usize, info.process_id, info.runtime_id))
}

fn log_installed_imes() {
    let imes = classify_installed_imes();
    if imes.is_empty() {
        tracing::warn!("no keyboard layouts enumerated");
        return;
    }
    tracing::info!("installed layouts:");
    for (hkl, lang, name, backend) in &imes {
        tracing::info!("  hkl=0x{hkl:08x} lang=0x{lang:04x} [{backend:?}] {name}");
    }
}

fn run_probe_shift() {
    use std::io::{BufRead, Write};
    println!("=== ImeFlow --probe-shift (pure SendInput, no IMM/profile/PostMessage) ===");
    println!("1) Manually select WeChat IME");
    println!("2) Focus the target window; confirm typing is currently 中文");
    println!("3) Press Enter here → one left-Shift (scancode) is injected");
    print!("ready> ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);

    let hwnd = process::foreground_hwnd();
    let root = process::root_hwnd(hwnd);
    let exe = process::hwnd_exe_name(root).unwrap_or_else(|| "?".into());
    let before = ImeController::read(root);
    println!(
        "fg={exe} imm_before={} (report only, not proof)",
        before.describe()
    );
    if let Some(info) = uia::inspect_focused() {
        println!(
            "uia kind={} class={} name={}",
            info.kind.as_str(),
            info.class_name,
            info.name
        );
    }

    match ime::tap_left_shift_scancode() {
        Ok(()) => println!("SendInput: 2 events inserted (left Shift scancode)"),
        Err(e) => {
            println!("SendInput failed: {e}");
            std::process::exit(2);
        }
    }
    std::thread::sleep(Duration::from_millis(120));
    let after = ImeController::read(root);
    println!(
        "imm_after={} evidence=imm_only effective_mode=unknown outcome=applied_unverified",
        after.describe()
    );
    println!("TYPE in the focused app now — did English actually work? (manual)");
    println!("PROBE_SHIFT_DONE");
}

fn run_probe_focus() {
    println!("=== ImeFlow probe-focus ===");
    println!("Focus a Codex text box / terminal, wait 1s…");
    std::thread::sleep(Duration::from_secs(1));
    let hwnd = process::foreground_hwnd();
    let root = process::root_hwnd(hwnd);
    let exe = process::hwnd_exe_name(root).unwrap_or_else(|| "?".into());
    println!("foreground exe: {exe}");
    match uia::inspect_focused() {
        Some(info) => {
            println!("kind:        {}", info.kind.as_str());
            println!("controlType: {}", info.control_type);
            println!("name:        {}", info.name);
            println!("class:       {}", info.class_name);
            println!("autoId:      {}", info.automation_id);
            println!("localized:   {}", info.localized_type);
            println!("ariaRole:    {}", info.aria_role);
            println!("password:    {}", info.is_password);
            println!("breadcrumb:  {}", info.breadcrumb);
            if let Ok(engine) = RuleEngine::load_from(&resolve_rules_path()) {
                let mode = engine.match_target(&exe, Some(info.kind));
                println!("rule → {mode:?}");
            }
            println!("PROBE_FOCUS_OK");
        }
        None => {
            println!("no focused UIA element");
            println!("PROBE_FOCUS_FAIL");
            std::process::exit(2);
        }
    }
}

fn run_probe() {
    println!("=== ImeFlow V0 probe ===");
    let imes = classify_installed_imes();
    println!("layouts ({}):", imes.len());
    for (hkl, lang, name, backend) in &imes {
        println!("  hkl=0x{hkl:08x} lang=0x{lang:04x} [{backend:?}] {name}");
    }

    let path = resolve_rules_path();
    match RuleEngine::load_from(&path) {
        Ok(e) => {
            println!("rules: {} from {}", e.len(), path.display());
            assert_eq!(
                e.match_process("Code.exe"),
                TargetMode::ImeEnglish,
                "Code.exe rule"
            );
            assert_eq!(
                e.match_process("WeChat.exe"),
                TargetMode::NativeChinese,
                "WeChat.exe rule"
            );
            assert_eq!(
                e.match_process("Weixin.exe"),
                TargetMode::NativeChinese,
                "Weixin.exe rule"
            );
            assert_eq!(
                e.match_process("notepad.exe"),
                TargetMode::NativeChinese,
                "notepad.exe rule"
            );
            println!("rule asserts: ok");
        }
        Err(e) => {
            println!("rules load error: {e}");
            std::process::exit(1);
        }
    }

    let hwnd = process::foreground_hwnd();
    let exe = process::hwnd_exe_name(hwnd).unwrap_or_else(|| "?".into());
    let before = ImeController::read(hwnd);
    println!("foreground: {exe}");
    println!("ime before: {}", before.describe());

    let target = match before.as_target_guess() {
        TargetMode::ImeEnglish => TargetMode::NativeChinese,
        TargetMode::NativeChinese => TargetMode::ImeEnglish,
        _ => TargetMode::NativeChinese,
    };
    println!("try apply → {target:?} (WeChat-first tip + conversion)");
    match ImeController::apply(hwnd, target) {
        Ok(after) => {
            println!("ime after:  {}", after.describe());
            let ok = matches_already(&after, target);
            println!("match_ok:   {ok}");
            let _ = ImeController::apply(hwnd, before.as_target_guess());
            println!("restored toward: {:?}", before.as_target_guess());
            if ok {
                println!("PROBE_OK");
            } else {
                // Conversion readback often fails on explorer; tip switch still counts for V0.
                let tip_ok = after.layout.backend != ActiveIme::Unknown
                    && after.layout.backend != ActiveIme::LayoutEnglish;
                if tip_ok && matches!(target, TargetMode::NativeChinese | TargetMode::ImeEnglish)
                {
                    println!("PROBE_OK_TIP_ONLY (conversion readback weak on this hwnd)");
                } else {
                    println!("PROBE_FAIL");
                    std::process::exit(2);
                }
            }
        }
        Err(e) => {
            println!("apply failed: {e}");
            println!("PROBE_FAIL");
            std::process::exit(2);
        }
    }
}

/// Simulate Alt+Tab between coding IDE and WeChat; requires imeflow daemon OR applies locally.
/// Prefer daemon: focus only, then readback after hook settles.
fn run_verify_switch() {
    println!("=== ImeFlow V0 verify-switch ===");
    let engine = match RuleEngine::load_from(&resolve_rules_path()) {
        Ok(e) => e,
        Err(e) => {
            println!("rules: {e}");
            std::process::exit(1);
        }
    };

    // Prefer real Code.exe; fall back to Cursor (this machine has no VS Code).
    let ide_exe = if process::find_hwnd_by_exe("Code.exe").is_some() {
        "Code.exe"
    } else if process::find_hwnd_by_exe("Cursor.exe").is_some() {
        println!("NOTE: Code.exe not found; using Cursor.exe as VS Code-class IDE");
        "Cursor.exe"
    } else {
        println!("VERIFY_FAIL: neither Code.exe nor Cursor.exe window found");
        std::process::exit(2);
    };

    let chat_exe = if process::find_hwnd_by_exe("WeChat.exe").is_some() {
        "WeChat.exe"
    } else if process::find_hwnd_by_exe("Weixin.exe").is_some() {
        "Weixin.exe"
    } else {
        println!("VERIFY_FAIL: WeChat/Weixin window not found");
        std::process::exit(2);
    };

    // Controllable Chinese partner: ChatGPT/Notepad when Weixin conversion APIs refuse writes.
    let alt_chinese = ["ChatGPT.exe", "notepad.exe"]
        .into_iter()
        .find(|exe| process::find_hwnd_by_exe(exe).is_some());

    let ide_hwnd = process::find_hwnd_by_exe(ide_exe).unwrap();
    let chat_hwnd = process::find_hwnd_by_exe(chat_exe).unwrap();
    let ide_mode = engine.match_process(ide_exe);
    let chat_mode = engine.match_process(chat_exe);
    println!("IDE  {ide_exe} → {ide_mode:?}");
    println!("Chat {chat_exe} → {chat_mode:?}");
    if let Some(alt) = alt_chinese {
        println!(
            "AltChinese {alt} → {:?}",
            engine.match_process(alt)
        );
    }

    let self_pid = std::process::id();
    let daemon_running = other_imeflow_running(self_pid);
    println!(
        "imeflow daemon: {}",
        if daemon_running {
            "RUNNING"
        } else {
            "not running (will apply locally)"
        }
    );

    let mut ide_fails = 0u32;
    let mut weixin_fails = 0u32;
    let mut alt_fails = 0u32;
    let alt_hwnd = alt_chinese.and_then(|exe| process::find_hwnd_by_exe(exe));
    let alt_mode = alt_chinese.map(|exe| engine.match_process(exe));

    for round in 1..=3 {
        println!("-- round {round} --");

        // 1) WeChat/Weixin → NativeChinese
        {
            let focused = process::focus_hwnd(chat_hwnd);
            println!("focus chat/{chat_exe}: {focused}");
            std::thread::sleep(std::time::Duration::from_millis(if daemon_running {
                900
            } else {
                500
            }));
            let fg = process::foreground_hwnd();
            if !daemon_running {
                if let Err(e) = ImeController::apply(fg, chat_mode) {
                    println!("  local apply err: {e}");
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let snap = ImeController::read(fg);
            let ok = matches_already(&snap, chat_mode);
            println!("  weixin ime={} expect={:?} ok={}", snap.describe(), chat_mode, ok);
            if !ok {
                weixin_fails += 1;
            }
        }

        // 2) IDE → ImeEnglish
        {
            let focused = process::focus_hwnd(ide_hwnd);
            println!("focus ide/{ide_exe}: {focused}");
            std::thread::sleep(std::time::Duration::from_millis(if daemon_running {
                900
            } else {
                500
            }));
            let fg = process::foreground_hwnd();
            if !daemon_running {
                if let Err(e) = ImeController::apply(fg, ide_mode) {
                    println!("  local apply err: {e}");
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let snap = ImeController::read(fg);
            let ok = matches_already(&snap, ide_mode);
            println!("  ide ime={} expect={:?} ok={}", snap.describe(), ide_mode, ok);
            if !ok {
                ide_fails += 1;
            }
        }

        // 3) Optional controllable Chinese host (ChatGPT/Notepad) — WeChat IME 中 path
        if let (Some(alt_exe), Some(hwnd), Some(mode)) = (alt_chinese, alt_hwnd, alt_mode) {
            let focused = process::focus_hwnd(hwnd);
            println!("focus alt/{alt_exe}: {focused}");
            std::thread::sleep(std::time::Duration::from_millis(if daemon_running {
                900
            } else {
                500
            }));
            let fg = process::foreground_hwnd();
            if !daemon_running {
                if let Err(e) = ImeController::apply(fg, mode) {
                    println!("  local apply err: {e}");
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let snap = ImeController::read(fg);
            let ok = matches_already(&snap, mode);
            println!("  alt ime={} expect={:?} ok={}", snap.describe(), mode, ok);
            if !ok {
                alt_fails += 1;
            }
        }
    }

    let ide_ok = ide_fails == 0;
    let weixin_ok = weixin_fails == 0;
    let alt_ok = alt_chinese.is_none() || alt_fails == 0;
    println!("summary ide_ok={ide_ok} weixin_ok={weixin_ok} alt_ok={alt_ok}");

    if ide_ok && weixin_ok {
        println!("VERIFY_OK");
        return;
    }

    println!("VERIFY_FAIL ide_fails={ide_fails} weixin_fails={weixin_fails} alt_fails={alt_fails}");
    if ide_ok && alt_ok && alt_chinese.is_some() {
        println!(
            "NOTE: IDE + {} NativeChinese works (WeChat IME path); Weixin shell still blocked",
            alt_chinese.unwrap()
        );
    }
    std::process::exit(2);
}

fn apply_with_transition_hint(
    root: HWND,
    target: TargetMode,
    kind: Option<uia::ControlKind>,
    resolved_layer: Option<TargetMode>,
    observation: ime::LayerObservation,
) -> Result<ime::ImeSnapshot, String> {
    if kind == Some(uia::ControlKind::Terminal) && target == TargetMode::ImeEnglish {
        // 调用方已用 plan_terminal_english 判定 Shift；此处只执行 quiet+一键。
        tracing::info!(
            ?observation,
            ?resolved_layer,
            "terminal: focus-quiet then one Shift → expected 英"
        );
    }
    ImeController::apply_for(root, target, kind)
}

fn on_watch(
    ev: WatchEvent,
    engine: &RuleEngine,
    bus: &FeedbackBus,
    last: &parking_lot::Mutex<LastMatch>,
) {
    let event_hwnd = match ev { WatchEvent::Foreground(h) | WatchEvent::Focus(h) => h };
    // Events are wakeups, not authoritative descriptions of the current focus.
    let fg = process::foreground_hwnd();
    let root = process::root_hwnd(fg);
    let generation = AppWatcher::generation();

    // Consume the lazy manual-Shift evidence before root-change cleanup. This preserves the
    // common path “terminal 英 → tap Shift for temporary 中 → immediately switch app”.
    let manual_chinese = key_trace::take_user_chinese_override();
    let root_changed = {
        let mut state = last.lock();
        if manual_chinese {
            state.ime_layer = Some(TargetMode::NativeChinese);
            state.expected_mode = None;
            state.block_chinese_until = None;
            state.cursor_terminal_guard_until = None;
            state.last_apply_at = None;
        }
        let changed = state.root != root.0 as isize;
        if changed {
            state.root = root.0 as isize;
            state.gate.reset();
            state.rechecks = 0;
            // Per-root expectation resets; global layer memory survives only as fallback evidence.
            state.expected_mode = None;
            state.cursor_terminal_seen_at = None;
            state.cursor_terminal_guard_until = None;
            state.block_chinese_until = None;
            state.last_apply_at = None;
        }
        if state.generation != generation {
            state.generation = generation;
            state.rechecks = 0;
        }
        changed
    };
    if manual_chinese {
        bus.set_last_mode(TargetMode::NativeChinese);
        tray::refresh();
        debug_ui::request_refresh();
        tracing::info!("user override: clean Shift tap → ime_layer 中");
    }
    // If no manual tap was consumed, leaving the root disarms the lazy listener so a Shift in
    // another app cannot rewrite terminal memory later.
    if root_changed {
        key_trace::clear_user_override();
    }

    if fg.0.is_null() { return; }
    let Some(exe) = process::hwnd_exe_name(root) else { return };
    if exe.eq_ignore_ascii_case("imeflow.exe") { return; }
    if root_changed {
        // Even when UIA is unavailable, never display the previous application's mode as current.
        bus.begin_focus(&format!("root:{:x}", root.0 as usize), &exe);
        tray::refresh();
        debug_ui::request_refresh();
    }
    if !tray::enabled() || bus.in_override_cooldown() { return; }
    // 暂停期间 gate 仍可能记着 AlreadyHandled；恢复后必须重看当前焦点。
    if RESYNC_AFTER_ENABLE.swap(false, Ordering::SeqCst) {
        let mut state = last.lock();
        state.gate.reset();
        state.expected_mode = None;
        state.rechecks = 0;
        state.last_apply_at = None;
        tracing::info!("tray: resync after auto re-enabled");
    }

    let native_focus = match process::InputTarget::capture(root) {
        Ok(value) => value,
        Err(err) => {
            tracing::debug!(%err, "waiting for native focus");
            request_focus_confirmation(last);
            return;
        }
    };
    let Some(focus) = uia::inspect_focused() else {
        tracing::debug!(%exe, "UIA unavailable; no process-only guess during focus transition");
        request_focus_confirmation(last);
        return;
    };
    let Some(base_key) = focus_key(root, &focus) else {
        tracing::debug!(%exe, uia_pid = focus.process_id, owner = focus.owner_root,
            "UIA identity missing/mismatched; no IME write");
        request_focus_confirmation(last);
        return;
    };
    if native_focus.ensure_current().is_err() {
        request_focus_confirmation(last);
        return;
    }

    let now = Instant::now();
    let cursor_like = exe.eq_ignore_ascii_case("Cursor.exe") || exe.eq_ignore_ascii_case("Code.exe");
    let raw_kind = focus.kind;
    let mut effective_kind = raw_kind;
    let mut sticky_terminal = false;

    if cursor_like {
        if raw_kind == uia::ControlKind::Terminal {
            let mut state = last.lock();
            state.cursor_terminal_seen_at.get_or_insert(now);
        } else if raw_kind == uia::ControlKind::Edit && !focus.looks_explicit_chat_input() {
            let terminalish = {
                let state = last.lock();
                let post_apply_guard = state.cursor_terminal_guard_until
                    .map(|until| now < until)
                    .unwrap_or(false);
                let pre_apply_handoff = state.cursor_terminal_seen_at
                    .map(|since| now.saturating_duration_since(since) < Duration::from_millis(1200))
                    .unwrap_or(false);
                post_apply_guard || pre_apply_handoff
            };
            if terminalish {
                // Cursor frequently reports xterm's helper textarea as a plain Edit both while
                // focus is settling and just after the terminal Shift. Keep both phases in one
                // logical terminal scope so an internal focus handoff cannot request Chinese.
                effective_kind = uia::ControlKind::Terminal;
                sticky_terminal = true;
                tracing::info!(%exe, control = %focus.summarize(),
                    "cursor terminal handoff: transient Edit kept as terminal");
            } else {
                last.lock().cursor_terminal_seen_at = None;
            }
        } else if focus.looks_explicit_chat_input() {
            let mut state = last.lock();
            state.cursor_terminal_seen_at = None;
            state.cursor_terminal_guard_until = None;
        } else {
            last.lock().cursor_terminal_seen_at = None;
        }

        if effective_kind == uia::ControlKind::Terminal {
            // Only wait for xterm's own focus handoff. Cross-app state is no longer inferred
            // from the previous app, so an extra foreground delay is unnecessary.
            let xterm_stable = last.lock().cursor_terminal_seen_at
                .map(|since| now.saturating_duration_since(since) >= Duration::from_millis(360))
                .unwrap_or(false);
            if !xterm_stable {
                tracing::debug!(%exe, "cursor terminal waiting for xterm focus to settle");
                request_focus_confirmation(last);
                return;
            }
        }
    }

    let key = if cursor_like && effective_kind == uia::ControlKind::Terminal {
        // One logical scope for xterm's native surface + helper textarea. RuntimeId churn inside
        // this scope must not create a second, opposite IME transaction.
        format!("{:x}:{}:integrated-terminal", root.0 as usize, focus.process_id)
    } else {
        base_key
    };
    let kind = Some(effective_kind);
    let target = engine.match_target(&exe, kind);
    let control_label = if sticky_terminal {
        format!("{} [raw=edit, effective=terminal]", focus.summarize())
    } else {
        focus.summarize()
    };
    // Display follows this logical region, not the last historical successful action.
    if bus.begin_focus(&key, &format!("{exe}/{}", effective_kind.as_str())) {
        debug_ui::request_refresh();
        tray::refresh();
    }
    let decision = last.lock().gate.observe(key.clone(), target, now);
    tracing::debug!(generation, ?event_hwnd, %exe, %key, ?raw_kind, ?effective_kind,
        ?target, ?decision, control = %control_label, "focus decision");
    match decision {
        focus_gate::Decision::Confirm => {
            request_focus_confirmation(last);
            return;
        }
        focus_gate::Decision::AlreadyHandled => {
            // Includes manual Shift changes in this same logical input element.
            return;
        }
        focus_gate::Decision::Ready => {},
    }

    // Terminal focus hops: block transient 中 after terminal→英; Cursor gets an extra 900ms guard.
    // Legitimate browser address-bar → page-input transitions must not inherit this guard.
    {
        let mut state = last.lock();
        let block_chinese = target == TargetMode::NativeChinese
            && !focus.looks_explicit_chat_input()
            && state
                .block_chinese_until
                .as_ref()
                .map(|(e, until)| e.eq_ignore_ascii_case(&exe) && now < *until)
                .unwrap_or(false);
        let opposite_cool = if cursor_like {
            match (state.expected_mode, state.last_apply_at) {
                (Some(prev), Some(at)) => {
                    prev.opposite_ime_layer(target)
                        && at.elapsed() < Duration::from_millis(900)
                        && !focus.looks_explicit_chat_input()
                }
                _ => false,
            }
        } else {
            false
        };
        if block_chinese || opposite_cool {
            tracing::info!(
                %exe,
                ?target,
                block_chinese,
                opposite_cool,
                "flicker guard: defer opposite layer write"
            );
            state.rechecks = 0;
            drop(state);
            request_focus_confirmation(last);
            return;
        }
    }

    if ime::modifiers_down() {
        tracing::debug!(%exe, "modifier held; defer without injecting keys");
        request_focus_confirmation(last);
        return;
    }
    let snap = ImeController::read(root);
    if !snap.auto_switch_enabled() {
        tracing::warn!(
            ime = snap.backend_label(),
            "active IME backend disabled for auto-switch; use Microsoft Pinyin"
        );
        last.lock().gate.complete(key.clone());
        bus.record_passthrough(&exe);
        tray::refresh();
        debug_ui::set_extra(|e| {
            e.foreground = exe.clone();
            e.control = control_label.clone();
            e.snapshot = snap.describe();
            e.last_action = format!("{exe}: skip auto-switch ({})", snap.backend_label());
        });
        return;
    }
    let ime_layer = last.lock().ime_layer;
    let observation = snap.observe_layer();
    let resolved_layer = snap.resolve_layer(ime_layer);
    // Cross-process queries can yield messages. Revalidate the logical element too.
    let latest = uia::inspect_focused();
    let still_same = latest.as_ref().map(|f| {
        if cursor_like && effective_kind == uia::ControlKind::Terminal {
            // During Cursor's xterm handoff, RuntimeId may change from terminal surface to a
            // helper Edit. Accept that as the same logical terminal unless it clearly looks
            // like a real chat/prompt input.
            focus_key(root, f).is_some()
                && (f.kind == uia::ControlKind::Terminal
                    || (f.kind == uia::ControlKind::Edit && !f.looks_explicit_chat_input()))
        } else {
            focus_key(root, f).as_ref() == Some(&key)
                && engine.match_target(&exe, Some(f.kind)) == target
        }
    }).unwrap_or(false);
    if !still_same || native_focus.ensure_current().is_err() || AppWatcher::generation() != generation {
        tracing::debug!(%exe, "stale focus decision cancelled before apply");
        request_focus_confirmation(last);
        return;
    }

    let terminal_plan = if effective_kind == uia::ControlKind::Terminal
        && target == TargetMode::ImeEnglish
    {
        Some(ime::plan_terminal_english(
            snap.layout.backend,
            observation,
            ime_layer,
        ))
    } else {
        None
    };
    match terminal_plan {
        Some(ime::TerminalEnPlan::Defer) => {
            tracing::info!(
                %exe,
                generation,
                ?observation,
                ?ime_layer,
                "terminal: Defer (insufficient mode evidence; not a focus error)"
            );
            bus.record_deferred(&format!("{exe}/terminal"), target);
            tray::refresh();
            debug_ui::set_extra(|e| {
                e.foreground = exe.clone();
                e.control = control_label.clone();
                e.snapshot = snap.describe();
                e.last_action = format!("{exe}: terminal Defer");
            });
            request_focus_confirmation(last);
            return;
        }
        Some(ime::TerminalEnPlan::Skip) => {
            let apply_id = {
                let mut state = last.lock();
                state.gate.complete(key.clone());
                state.expected_mode = Some(target);
                if let ime::LayerObservation::Confident(observed_mode) = observation {
                    // An already-active ENG layout is not the Chinese IME's English layer.
                    state.ime_layer = Some(observed_mode);
                }
                state.next_apply += 1;
                state.next_apply
            };
            let _ = apply_id;
            tracing::info!(?observation, "terminal: Skip (Confident 英)");
            bus.record_skip_same(&format!("{exe}/terminal"), target, true);
            tray::refresh();
            debug_ui::set_extra(|e| {
                e.foreground = exe.clone();
                e.control = control_label.clone();
                e.snapshot = snap.describe();
                e.last_action = format!("{exe}: terminal already 英");
            });
            return;
        }
        Some(ime::TerminalEnPlan::Shift) | None => {}
    }

    // Consume this focus epoch before any IME API. Queued self-events cannot reapply it.
    let apply_id = {
        let mut state = last.lock();
        state.gate.complete(key.clone());
        state.next_apply += 1;
        state.next_apply
    };
    let span = tracing::info_span!("ime_apply", apply_id, generation, exe = %exe,
        target = %target, scope = %key);
    let _entered = span.enter();
    debug_ui::set_extra(|e| {
        e.foreground = exe.clone();
        e.control = control_label.clone();
        e.snapshot = snap.describe();
        e.enabled = true;
    });
    if target == TargetMode::Passthrough {
        tracing::info!(?ime_layer, ?observation, "passthrough; no writes; ime_layer kept");
        bus.record_passthrough(&exe);
        tray::refresh();
        debug_ui::set_extra(|e| e.last_action = format!("{exe}/{control_label}: passthrough"));
        return;
    }
    // Resolved live/memory layer already matches target → no write.
    // terminal→英 已由 plan_terminal_english 处理。
    if !(effective_kind == uia::ControlKind::Terminal && target == TargetMode::ImeEnglish)
        && matches!(target, TargetMode::NativeChinese | TargetMode::ImeEnglish)
        && resolved_layer == Some(target)
    {
        tracing::info!(
            ?target,
            ?observation,
            ?resolved_layer,
            "resolved layer already matches; no write"
        );
        last.lock().expected_mode = Some(target);
        if matches!(observation, ime::LayerObservation::Confident(_)) {
            last.lock().ime_layer = Some(target);
        }
        let confident = matches!(observation, ime::LayerObservation::Confident(m) if m == target);
        bus.record_skip_same(&format!("{exe}/{}", effective_kind.as_str()), target, confident);
        tray::refresh();
        debug_ui::set_extra(|e| e.last_action = format!("{exe}: resolved already {target}"));
        return;
    }
    // Only a real keyboard-layout match is trusted enough to skip a write.
    if target == TargetMode::LayoutEnglish && matches_already(&snap, target) {
        tracing::info!("layout report matches; no write");
        last.lock().expected_mode = Some(target);
        bus.record_skip_same(&format!("{exe}/{}", effective_kind.as_str()), target, true);
        tray::refresh();
        debug_ui::set_extra(|e| e.last_action = format!("{exe}: layout already {target}"));
        return;
    }
    let before = snap.describe();
    let kind_s = effective_kind.as_str();
    tracing::info!(
        %before,
        ?observation,
        ?resolved_layer,
        ?raw_kind,
        ?effective_kind,
        control = %control_label,
        "apply begin"
    );
    let will_terminal_shift = effective_kind == uia::ControlKind::Terminal
        && target == TargetMode::ImeEnglish;
    if will_terminal_shift && TRACE_KEYS.load(Ordering::SeqCst) {
        key_trace::arm_shift_window(apply_id, Duration::from_millis(500));
    }
    match apply_with_transition_hint(root, target, kind, resolved_layer, observation) {
        Ok(after_snap) => {
            let after_obs = after_snap.observe_layer();
            {
                let mut state = last.lock();
                // Restore the last user-verified stable behavior: transition memory tracks the
                // action we intentionally performed. UI/metrics still keep a separate confirmed flag.
                state.expected_mode = Some(target);
                match after_obs {
                    ime::LayerObservation::Confident(m)
                        if matches!(
                            m,
                            TargetMode::NativeChinese
                                | TargetMode::ImeEnglish
                                | TargetMode::LayoutEnglish
                        ) =>
                    {
                        state.ime_layer = Some(m);
                    }
                    ime::LayerObservation::Unknown | ime::LayerObservation::Probable(_)
                        if will_terminal_shift
                            || resolved_layer
                                .map(|r| r.opposite_ime_layer(target))
                                .unwrap_or(false)
                            || (target == TargetMode::NativeChinese && resolved_layer.is_none()) =>
                    {
                        state.ime_layer = Some(target);
                    }
                    _ => {}
                }
                if will_terminal_shift {
                    state.last_apply_at = Some(Instant::now());
                    state.block_chinese_until =
                        Some((exe.clone(), Instant::now() + Duration::from_millis(1600)));
                    if cursor_like {
                        state.cursor_terminal_guard_until =
                            Some(Instant::now() + Duration::from_millis(900));
                    }
                } else {
                    state.last_apply_at = Some(Instant::now());
                    if target == TargetMode::NativeChinese {
                        state.block_chinese_until = None;
                    }
                    if cursor_like && focus.looks_explicit_chat_input() {
                        state.cursor_terminal_guard_until = None;
                        state.cursor_terminal_seen_at = None;
                    }
                }
            }
            if will_terminal_shift {
                // Stable behavior: after the one terminal Shift, listen for a clean physical
                // Shift so the user can temporarily override back to Chinese.
                key_trace::arm_user_override(Duration::from_secs(300));
            }
            let after = after_snap.describe();
            // Reporting uses the same confidence contract in EVERY control type. A raw IMM
            // match must not become 'confirmed' merely because this is an edit/address bar.
            let imm_matches = matches_already(&after_snap, target);
            let confirmed = matches!(after_obs, ime::LayerObservation::Confident(m) if m == target)
                && process::InputTarget::capture(root).is_ok();
            let focus_hwnd = process::InputTarget::capture(root)
                .map(|t| format!("{:x}", t.focus as usize))
                .unwrap_or_else(|_| "?".into());
            let note = format!(
                "apply={apply_id} {kind_s} imm_report_matches={imm_matches} confirmed={confirmed} focus={focus_hwnd}"
            );
            tracing::info!(
                imm_after = %after,
                imm_report_matches = imm_matches,
                confirmed,
                %focus_hwnd,
                "apply end; no automatic retry"
            );
            if will_terminal_shift && TRACE_KEYS.load(Ordering::SeqCst) {
                finish_terminal_shift_trace(apply_id);
            }
            bus.record_switch(SwitchEvent {
                process: format!("{exe}/{kind_s}"),
                target,
                applied: true,
                confirmed,
            });
            // Count as effective only if the user types next without first correcting via Shift.
            key_trace::arm_effective_check(Duration::from_secs(45));
            tray::refresh();
            debug_ui::set_extra(|e| {
                e.snapshot = after;
                e.last_action = format!("{exe}/{kind_s} -> {target} | {note}");
            });
            debug_ui::notify_switch(confirmed);
        }
        Err(err) => {
            {
                let mut state = last.lock();
                state.expected_mode = None;
                if cursor_like && effective_kind == uia::ControlKind::Terminal {
                    state.cursor_terminal_guard_until = None;
                }
            }
            if will_terminal_shift && TRACE_KEYS.load(Ordering::SeqCst) {
                key_trace::cancel_shift_window();
            }
            tracing::warn!(%err, "apply stopped; no fallback layout or retry");
            bus.record_switch(SwitchEvent {
                process: format!("{exe}/{kind_s}"),
                target,
                applied: false,
                confirmed: false,
            });
            tray::refresh();
            debug_ui::set_extra(|e| e.last_action = format!("{exe}/{kind_s} STOP: {err}"));
        }
    }
}

fn finish_terminal_shift_trace(apply_id: u64) {
    if TRACE_PROBE.load(Ordering::SeqCst) {
        match ime::tap_probe_letter_b() {
            Ok(()) => tracing::info!(
                apply_id,
                "trace-probe: typed tagged 'b' — watch terminal for ASCII vs composition"
            ),
            Err(e) => tracing::warn!(apply_id, %e, "trace-probe skipped"),
        }
    }
    if let Some(report) = key_trace::finish_shift_window() {
        tracing::info!(
            apply_id,
            foreign_shift = report.foreign_shift(),
            summary = %report.summarize(),
            "key_trace: Shift watch closed"
        );
    } else {
        tracing::warn!(apply_id, "key_trace: Shift watch missing at finish");
    }
}

fn other_imeflow_running(self_pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::System::ProcessStatus::K32EnumProcesses;

    let mut pids = vec![0u32; 1024];
    let mut bytes = 0u32;
    unsafe {
        if !K32EnumProcesses(pids.as_mut_ptr(), (pids.len() * 4) as u32, &mut bytes).as_bool() {
            return false;
        }
    }
    let n = (bytes as usize) / 4;
    for &pid in &pids[..n] {
        if pid == 0 || pid == self_pid {
            continue;
        }
        unsafe {
            let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                continue;
            };
            let mut name = [0u16; 260];
            let mut size = name.len() as u32;
            let ok = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                windows::core::PWSTR(name.as_mut_ptr()),
                &mut size,
            )
            .is_ok();
            let _ = CloseHandle(handle);
            if !ok || size == 0 {
                continue;
            }
            let path = String::from_utf16_lossy(&name[..size as usize]);
            let file = std::path::Path::new(&path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            if file.eq_ignore_ascii_case("imeflow.exe") {
                return true;
            }
        }
    }
    false
}

fn matches_already(snap: &ime::ImeSnapshot, target: TargetMode) -> bool {
    match target {
        TargetMode::LayoutEnglish => snap.layout.backend == ActiveIme::LayoutEnglish,
        TargetMode::NativeChinese => {
            matches!(
                snap.layout.backend,
                ActiveIme::WeChatIme | ActiveIme::MsPinyin | ActiveIme::OtherChinese
            ) && snap.conversion_native == Some(true)
        }
        TargetMode::ImeEnglish => {
            matches!(
                snap.layout.backend,
                ActiveIme::WeChatIme | ActiveIme::MsPinyin | ActiveIme::OtherChinese
            ) && snap.conversion_native == Some(false)
        }
        TargetMode::Passthrough => true,
    }
}

fn ctrlc_stub() {
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::System::Console::{SetConsoleCtrlHandler, CTRL_C_EVENT};
    unsafe extern "system" fn handler(ctrl: u32) -> BOOL {
        if ctrl == CTRL_C_EVENT {
            tracing::info!("Ctrl+C — exiting");
            AppWatcher::quit();
            BOOL(1)
        } else {
            BOOL(0)
        }
    }
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(handler), true);
    }
}
