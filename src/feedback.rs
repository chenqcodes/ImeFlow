//! Reporting is separate from the IME planner's soft memory.
//! A requested target is never itself evidence of the current typing mode.
use crate::mode::TargetMode;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use windows::Win32::System::SystemInformation::GetLocalTime;

const MAX_EVENTS: usize = 50;

#[derive(Debug, Clone)]
pub struct SwitchEvent {
    pub process: String,
    pub target: TargetMode,
    pub applied: bool,
    pub confirmed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind { Waiting, Confirmed, Assumed, Requested, Deferred, Failed, Passthrough }

#[derive(Debug, Clone)]
pub struct StatusView {
    pub process: String,
    pub target: Option<TargetMode>,
    pub kind: StatusKind,
}
impl Default for StatusView {
    fn default() -> Self { Self { process: String::new(), target: None, kind: StatusKind::Waiting } }
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    /// Confirmed automatic requests, not a guarantee of saved keystrokes.
    pub today_auto_switch: u64,
    pub total_auto_switch: u64,
    /// Applied switch followed by typing without a corrective Shift first.
    pub today_effective: u64,
    pub total_effective: u64,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct PersistentStats {
    day: String,
    today_auto_switch: u64,
    total_auto_switch: u64,
    today_effective: u64,
    total_effective: u64,
}

struct Inner {
    persisted: PersistentStats,
    persist_path: PathBuf,
    persistence_enabled: bool,
    events: VecDeque<SwitchEvent>,
    scope: String,
    current: StatusView,
    override_until: Option<Instant>,
}
#[derive(Clone)]
pub struct FeedbackBus { inner: Arc<Mutex<Inner>> }
impl Default for FeedbackBus {
    fn default() -> Self { Self::from_path(stats_path()) }
}

impl FeedbackBus {
    fn from_path(path: PathBuf) -> Self {
        let (mut persisted, persistence_enabled) = match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<PersistentStats>(&text) {
                Ok(value) => (value, true),
                Err(error) => {
                    // Preserve the bad file before permitting a new statistics file.
                    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default().as_nanos();
                    let backup = path.with_extension(format!("corrupt-{stamp}.toml"));
                    let backed_up = std::fs::copy(&path, &backup).is_ok();
                    tracing::warn!(%error, path = %path.display(), backup = %backup.display(), backed_up,
                        "stats: invalid data; start in-memory counters, preserve original");
                    (PersistentStats::default(), backed_up)
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (PersistentStats::default(), true),
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "stats: unreadable; persistence disabled for this session");
                (PersistentStats::default(), false)
            }
        };
        rollover(&mut persisted, &local_day());
        Self { inner: Arc::new(Mutex::new(Inner {
            persisted, persist_path: path, persistence_enabled,
            events: VecDeque::new(), scope: String::new(), current: StatusView::default(), override_until: None,
        })) }
    }

    /// Reset only the display when the logical input region changes. Never alters the planner.
    pub fn begin_focus(&self, scope: &str, process: &str) -> bool {
        let mut g = self.inner.lock();
        if g.scope == scope { return false; }
        g.scope = scope.into();
        g.current = StatusView { process: process.into(), ..Default::default() };
        true
    }

    pub fn record_passthrough(&self, process: &str) {
        self.set_status(process, None, StatusKind::Passthrough);
    }
    pub fn record_deferred(&self, process: &str, target: TargetMode) {
        self.set_status(process, Some(target), StatusKind::Deferred);
    }
    pub fn record_skip_same(&self, process: &str, mode: TargetMode, confident: bool) {
        self.set_status(process, Some(mode), if confident { StatusKind::Confirmed } else { StatusKind::Assumed });
    }
    pub fn set_last_mode(&self, mode: TargetMode) {
        let mut g = self.inner.lock();
        // Physical Shift direction is inferred from a soft baseline, not independently read.
        g.current.target = Some(mode);
        g.current.kind = StatusKind::Assumed;
    }
    fn set_status(&self, process: &str, target: Option<TargetMode>, kind: StatusKind) {
        let mut g = self.inner.lock();
        g.current = StatusView { process: process.into(), target, kind };
    }

    pub fn record_switch(&self, mut ev: SwitchEvent) {
        let mut g = self.inner.lock();
        let day_changed = rollover(&mut g.persisted, &local_day());
        // Guard even contradictory call sites: confirmed=true + applied=false cannot count.
        ev.confirmed = ev.confirmed && ev.applied && ev.target != TargetMode::Passthrough;
        if ev.confirmed {
            g.persisted.today_auto_switch = g.persisted.today_auto_switch.saturating_add(1);
            g.persisted.total_auto_switch = g.persisted.total_auto_switch.saturating_add(1);
        }
        g.current = StatusView {
            process: ev.process.clone(), target: Some(ev.target),
            kind: if ev.target == TargetMode::Passthrough { StatusKind::Passthrough }
                else if !ev.applied { StatusKind::Failed }
                else if ev.confirmed { StatusKind::Confirmed } else { StatusKind::Requested },
        };
        let save = day_changed || ev.confirmed;
        g.events.push_front(ev);
        while g.events.len() > MAX_EVENTS { g.events.pop_back(); }
        // Serialize persistence under the same lock. An older snapshot cannot overwrite a newer
        // one; temp+rename avoids truncating the existing TOML on interruption. No paint-path I/O.
        if save && g.persistence_enabled {
            if let Err(error) = persist_to(&g.persist_path, &g.persisted) {
                tracing::warn!(%error, "stats: persistence failed; in-memory counters retained");
            }
        }
    }

    #[cfg(test)]
    pub fn ui_snapshot(&self) -> (Stats, StatusView) {
        let g = self.inner.lock();
        (visible_stats(&g.persisted, &local_day()), g.current.clone())
    }
    pub fn in_override_cooldown(&self) -> bool {
        self.inner.lock().override_until.map(|until| Instant::now() < until).unwrap_or(false)
    }
    pub fn arm_override(&self, dur: std::time::Duration) {
        self.inner.lock().override_until = Instant::now().checked_add(dur);
    }
    pub fn last_status(&self) -> (String, Option<TargetMode>, bool, u64, u64) {
        let g = self.inner.lock();
        let stats = visible_stats(&g.persisted, &local_day());
        let mode = (g.current.kind == StatusKind::Confirmed).then_some(g.current.target).flatten();
        (g.current.process.clone(), mode,
            g.override_until.map(|t| Instant::now() < t).unwrap_or(false),
            stats.today_auto_switch, stats.total_auto_switch)
    }

    pub fn tip_stats(&self) -> (String, Stats) {
        let g = self.inner.lock();
        (g.current.process.clone(), visible_stats(&g.persisted, &local_day()))
    }

    /// User typed after an auto-switch without first correcting with Shift.
    pub fn record_effective(&self) {
        let mut g = self.inner.lock();
        let _ = rollover(&mut g.persisted, &local_day());
        g.persisted.today_effective = g.persisted.today_effective.saturating_add(1);
        g.persisted.total_effective = g.persisted.total_effective.saturating_add(1);
        tracing::info!(
            today = g.persisted.today_effective,
            total = g.persisted.total_effective,
            "stats: effective switch (+1)"
        );
        if g.persistence_enabled {
            if let Err(error) = persist_to(&g.persist_path, &g.persisted) {
                tracing::warn!(%error, "stats: persistence failed; in-memory counters retained");
            }
        }
    }

    /// Best-known 中/英 for the tray glyph. Includes Assumed/Requested so the icon
    /// is not stuck on gray until IMM happens to return Confident.
    pub fn tray_mode(&self) -> Option<TargetMode> {
        let g = self.inner.lock();
        match g.current.kind {
            StatusKind::Confirmed
            | StatusKind::Assumed
            | StatusKind::Requested
            | StatusKind::Deferred => g.current.target.filter(|m| {
                matches!(
                    m,
                    TargetMode::NativeChinese
                        | TargetMode::ImeEnglish
                        | TargetMode::LayoutEnglish
                )
            }),
            StatusKind::Waiting
            | StatusKind::Failed
            | StatusKind::Passthrough => None,
        }
    }
}

fn visible_stats(stats: &PersistentStats, day: &str) -> Stats {
    Stats {
        today_auto_switch: if stats.day == day { stats.today_auto_switch } else { 0 },
        total_auto_switch: stats.total_auto_switch,
        today_effective: if stats.day == day { stats.today_effective } else { 0 },
        total_effective: stats.total_effective,
    }
}
fn rollover(stats: &mut PersistentStats, day: &str) -> bool {
    if stats.day == day { return false; }
    stats.day = day.into();
    stats.today_auto_switch = 0;
    stats.today_effective = 0;
    true
}
fn stats_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir).join("ImeFlow").join("stats.toml")
}
fn persist_to(path: &Path, stats: &PersistentStats) -> Result<(), String> {
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
    let text = toml::to_string(stats).map_err(|e| e.to_string())?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() { let _ = std::fs::remove_file(&temporary); }
    result.map_err(|e| e.to_string())
}
fn local_day() -> String {
    let st = unsafe { GetLocalTime() };
    format!("{:04}-{:02}-{:02}", st.wYear, st.wMonth, st.wDay)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temporary() -> PathBuf {
        std::env::temp_dir().join(format!("imeflow-stats-{}-{}.toml", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()))
    }
    fn event(applied: bool, confirmed: bool) -> SwitchEvent {
        SwitchEvent { process: "Cursor.exe/terminal".into(), target: TargetMode::ImeEnglish, applied, confirmed }
    }
    #[test]
    fn confirmed_actions_persist_and_replace_existing_file() {
        let path = temporary(); let bus = FeedbackBus::from_path(path.clone());
        bus.record_switch(event(true, true)); bus.record_switch(event(true, true));
        assert_eq!(bus.ui_snapshot().0.today_auto_switch, 2);
        assert_eq!(FeedbackBus::from_path(path.clone()).ui_snapshot().0.total_auto_switch, 2);
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn invalid_unconfirmed_skip_and_passthrough_never_count() {
        let path = temporary(); let bus = FeedbackBus::from_path(path.clone());
        bus.record_switch(event(false, true)); bus.record_switch(event(true, false));
        let mut pass = event(true, true); pass.target = TargetMode::Passthrough; bus.record_switch(pass);
        bus.record_skip_same("test", TargetMode::ImeEnglish, false);
        assert_eq!(bus.last_status().1, None);
        bus.record_passthrough("explorer.exe");
        assert_eq!(bus.ui_snapshot().0.today_auto_switch, 0);
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn new_focus_and_failure_do_not_display_previous_success() {
        let path = temporary(); let bus = FeedbackBus::from_path(path.clone());
        bus.record_switch(event(true, true));
        assert!(bus.begin_focus("different-scope", "Edge.exe/edit"));
        assert_eq!(bus.ui_snapshot().1.kind, StatusKind::Waiting);
        assert_eq!(bus.last_status().1, None);
        bus.record_switch(event(false, false));
        assert_eq!(bus.ui_snapshot().1.kind, StatusKind::Failed);
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn tray_mode_includes_assumed_and_requested() {
        let path = temporary();
        let bus = FeedbackBus::from_path(path.clone());
        assert_eq!(bus.tray_mode(), None);
        bus.record_skip_same("Cursor.exe/edit", TargetMode::NativeChinese, false);
        assert_eq!(bus.tray_mode(), Some(TargetMode::NativeChinese));
        assert_eq!(bus.last_status().1, None); // still not Confirmed
        bus.record_skip_same("Cursor.exe/terminal", TargetMode::ImeEnglish, true);
        assert_eq!(bus.tray_mode(), Some(TargetMode::ImeEnglish));
        assert_eq!(bus.last_status().1, Some(TargetMode::ImeEnglish));
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn effective_counter_persists_and_rolls_today() {
        let path = temporary();
        let bus = FeedbackBus::from_path(path.clone());
        bus.record_effective();
        bus.record_effective();
        let (_, stats) = bus.tip_stats();
        assert_eq!(stats.today_effective, 2);
        assert_eq!(stats.total_effective, 2);
        assert_eq!(stats.today_auto_switch, 0);
        let again = FeedbackBus::from_path(path.clone());
        assert_eq!(again.tip_stats().1.total_effective, 2);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rollover_and_counter_limits_are_explicit() {
        let mut stats = PersistentStats {
            day: "yesterday".into(),
            today_auto_switch: 7,
            total_auto_switch: 50,
            today_effective: 3,
            total_effective: 20,
        };
        assert_eq!(visible_stats(&stats, "today").today_auto_switch, 0);
        assert_eq!(visible_stats(&stats, "today").today_effective, 0);
        assert!(rollover(&mut stats, "today"));
        assert_eq!(stats.total_auto_switch, 50);
        assert_eq!(stats.total_effective, 20);
        assert_eq!(stats.today_effective, 0);
        assert!(!rollover(&mut stats, "today"));
        let path = temporary(); let bus = FeedbackBus::from_path(path.clone());
        bus.inner.lock().persisted.total_auto_switch = u64::MAX;
        bus.record_switch(event(true, true));
        assert_eq!(bus.ui_snapshot().0.total_auto_switch, u64::MAX);
        let _ = std::fs::remove_file(path);
    }
}
