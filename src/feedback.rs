//! Reporting is separate from the IME planner's soft memory.
//! A requested target is never itself evidence of the current typing mode.
use crate::ime::LayerObservation;
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
pub enum StatusKind {
    Waiting,
    Confirmed,
    /// Shift/IMM write executed but IMM did not confirm (typical on WeChat).
    Applied,
    /// Skip because IMM/memory said already matched — may be a false read.
    Assumed,
    Requested,
    Deferred,
    Failed,
    Passthrough,
}

#[derive(Debug, Clone)]
pub struct StatusView {
    pub process: String,
    pub target: Option<TargetMode>,
    pub kind: StatusKind,
}
impl Default for StatusView {
    fn default() -> Self {
        Self {
            process: String::new(),
            target: None,
            kind: StatusKind::Waiting,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    /// Confirmed automatic requests, not a guarantee of saved keystrokes.
    pub today_auto_switch: u64,
    pub total_auto_switch: u64,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct PersistentStats {
    day: String,
    today_auto_switch: u64,
    total_auto_switch: u64,
}

struct Inner {
    persisted: PersistentStats,
    persist_path: PathBuf,
    persistence_enabled: bool,
    events: VecDeque<SwitchEvent>,
    scope: String,
    current: StatusView,
    override_until: Option<Instant>,
    /// Display-only observations never feed the planner or count as automatic switches.
    observed: Option<LayerObservation>,
    revision: u64,
}
#[derive(Clone)]
pub struct FeedbackBus {
    inner: Arc<Mutex<Inner>>,
}
impl Default for FeedbackBus {
    fn default() -> Self {
        Self::from_path(stats_path())
    }
}

impl FeedbackBus {
    fn from_path(path: PathBuf) -> Self {
        let (mut persisted, persistence_enabled) = match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<PersistentStats>(&text) {
                Ok(value) => (value, true),
                Err(error) => {
                    // Preserve the bad file before permitting a new statistics file.
                    let stamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos();
                    let backup = path.with_extension(format!("corrupt-{stamp}.toml"));
                    let backed_up = std::fs::copy(&path, &backup).is_ok();
                    tracing::warn!(%error, path = %path.display(), backup = %backup.display(), backed_up,
                        "stats: invalid data; start in-memory counters, preserve original");
                    (PersistentStats::default(), backed_up)
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                (PersistentStats::default(), true)
            }
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "stats: unreadable; persistence disabled for this session");
                (PersistentStats::default(), false)
            }
        };
        rollover(&mut persisted, &local_day());
        Self {
            inner: Arc::new(Mutex::new(Inner {
                persisted,
                persist_path: path,
                persistence_enabled,
                events: VecDeque::new(),
                scope: String::new(),
                current: StatusView::default(),
                override_until: None,
                observed: None,
                revision: 0,
            })),
        }
    }

    /// Reset only the display when the logical input region changes. Never alters the planner.
    pub fn begin_focus(&self, scope: &str, process: &str) -> bool {
        let mut g = self.inner.lock();
        if g.scope == scope {
            return false;
        }
        g.scope = scope.into();
        invalidate_observation(&mut g);
        g.current = StatusView {
            process: process.into(),
            ..Default::default()
        };
        true
    }

    pub fn record_passthrough(&self, process: &str) {
        self.set_status(process, None, StatusKind::Passthrough);
    }
    pub fn record_deferred(&self, process: &str, target: TargetMode) {
        self.set_status(process, Some(target), StatusKind::Deferred);
    }
    pub fn record_requested(&self, process: &str, target: TargetMode) {
        self.set_status(process, Some(target), StatusKind::Requested);
    }
    pub fn record_skip_same(&self, process: &str, mode: TargetMode, confident: bool) {
        self.set_status(
            process,
            Some(mode),
            if confident {
                StatusKind::Confirmed
            } else {
                StatusKind::Assumed
            },
        );
    }
    pub fn set_last_mode(&self, mode: TargetMode) {
        let mut g = self.inner.lock();
        invalidate_observation(&mut g);
        // Physical Shift direction is inferred from a soft baseline, not independently read.
        g.current.target = Some(mode);
        g.current.kind = StatusKind::Assumed;
    }
    fn set_status(&self, process: &str, target: Option<TargetMode>, kind: StatusKind) {
        let mut g = self.inner.lock();
        invalidate_observation(&mut g);
        g.current = StatusView {
            process: process.into(),
            target,
            kind,
        };
    }

    pub fn record_switch(&self, mut ev: SwitchEvent) {
        let mut g = self.inner.lock();
        invalidate_observation(&mut g);
        let day_changed = rollover(&mut g.persisted, &local_day());
        // Guard even contradictory call sites: confirmed=true + applied=false cannot count.
        ev.confirmed = ev.confirmed && ev.applied && ev.target != TargetMode::Passthrough;
        if ev.confirmed {
            g.persisted.today_auto_switch = g.persisted.today_auto_switch.saturating_add(1);
            g.persisted.total_auto_switch = g.persisted.total_auto_switch.saturating_add(1);
        }
        g.current = StatusView {
            process: ev.process.clone(),
            target: Some(ev.target),
            kind: if ev.target == TargetMode::Passthrough {
                StatusKind::Passthrough
            } else if !ev.applied {
                StatusKind::Failed
            } else if ev.confirmed {
                StatusKind::Confirmed
            } else {
                StatusKind::Applied
            },
        };
        let save = day_changed || ev.confirmed;
        g.events.push_front(ev);
        while g.events.len() > MAX_EVENTS {
            g.events.pop_back();
        }
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
        self.inner
            .lock()
            .override_until
            .map(|until| Instant::now() < until)
            .unwrap_or(false)
    }
    pub fn arm_override(&self, dur: std::time::Duration) {
        self.inner.lock().override_until = Instant::now().checked_add(dur);
    }
    #[cfg(test)]
    pub fn last_status(&self) -> (String, Option<TargetMode>, bool, u64, u64) {
        let g = self.inner.lock();
        let stats = visible_stats(&g.persisted, &local_day());
        let mode = (g.current.kind == StatusKind::Confirmed)
            .then_some(g.current.target)
            .flatten();
        (
            g.current.process.clone(),
            mode,
            g.override_until
                .map(|t| Instant::now() < t)
                .unwrap_or(false),
            stats.today_auto_switch,
            stats.total_auto_switch,
        )
    }

    pub fn tip_stats(&self) -> (String, Stats) {
        let g = self.inner.lock();
        (
            g.current.process.clone(),
            visible_stats(&g.persisted, &local_day()),
        )
    }

    pub fn observation_revision(&self) -> u64 {
        self.inner.lock().revision
    }

    /// Reject reads started before a focus/status transition or an automatic write.
    pub fn record_observation(&self, revision: u64, observation: LayerObservation) -> bool {
        let mut g = self.inner.lock();
        if g.revision != revision || g.observed == Some(observation) {
            return false;
        }
        g.observed = Some(observation);
        true
    }

    fn tray_icon_kind(kind: StatusKind) -> bool {
        matches!(kind, StatusKind::Confirmed | StatusKind::Applied)
    }

    /// Mode worth remembering across the next focus hop.
    /// In-flight Requested and passthrough must not overwrite it.
    pub fn tray_remembered_mode(&self) -> Option<TargetMode> {
        if let Some(observed) = self.inner.lock().observed {
            return observation_icon(observed);
        }
        if let Some(mode) = self.tray_mode() {
            return Some(mode);
        }
        let g = self.inner.lock();
        if g.current.kind == StatusKind::Assumed {
            return tray_target_icon(g.current.target);
        }
        None
    }

    /// Tray icon: Confirmed or Applied (we did shift/write) lights 中/英.
    pub fn tray_mode(&self) -> Option<TargetMode> {
        let g = self.inner.lock();
        if !Self::tray_icon_kind(g.current.kind) {
            return None;
        }
        tray_target_icon(g.current.target)
    }

    /// A requested target is not a reading. While settling or after an unconfirmed
    /// write, stay neutral until a live observation arrives.
    pub fn tray_icon_mode(&self, last_lit: Option<TargetMode>) -> Option<TargetMode> {
        let g = self.inner.lock();
        if let Some(observed) = g.observed {
            return observation_icon(observed);
        }
        if matches!(
            g.current.kind,
            StatusKind::Requested | StatusKind::Deferred | StatusKind::Applied
        ) {
            return None;
        }
        let fallback = last_lit.unwrap_or(TargetMode::NativeChinese);
        if let Some(mode) =
            tray_target_icon(g.current.target).filter(|_| Self::tray_icon_kind(g.current.kind))
        {
            return Some(mode);
        }
        if g.current.kind == StatusKind::Assumed {
            if let Some(mode) = tray_target_icon(g.current.target) {
                return Some(mode);
            }
        }
        // Waiting / passthrough / 失败：保持上次，没有上次就默认中文。
        Some(fallback)
    }

    /// Tooltip mode line.
    pub fn tray_mode_label(&self) -> String {
        let g = self.inner.lock();
        if let Some(observed) = g.observed {
            return match observed {
                LayerObservation::Confident(mode) => {
                    format!("当前·{}", target_short_label(Some(mode)))
                }
                LayerObservation::Probable(mode) => format!(
                    "推测·{}（实时读取，未确认）",
                    target_short_label(Some(mode))
                ),
                LayerObservation::Unknown => "当前状态未知".into(),
            };
        }
        let tag = target_short_label(g.current.target);
        match g.current.kind {
            StatusKind::Confirmed => tag,
            StatusKind::Applied => format!("已请求·{tag}（未确认）"),
            StatusKind::Assumed => format!("推测·{tag}（未确认）"),
            StatusKind::Requested | StatusKind::Deferred => {
                if tag == "中+E" {
                    "待确认".into()
                } else {
                    format!("待确认·{tag}")
                }
            }
            StatusKind::Waiting => "…".into(),
            StatusKind::Failed => "失败".into(),
            StatusKind::Passthrough => "—".into(),
        }
    }
}

fn invalidate_observation(inner: &mut Inner) {
    inner.revision = inner.revision.wrapping_add(1);
    inner.observed = None;
}

fn observation_icon(observation: LayerObservation) -> Option<TargetMode> {
    match observation {
        LayerObservation::Confident(mode) | LayerObservation::Probable(mode) => {
            tray_target_icon(Some(mode))
        }
        LayerObservation::Unknown => None,
    }
}

fn tray_target_icon(target: Option<TargetMode>) -> Option<TargetMode> {
    target.filter(|m| {
        matches!(
            m,
            TargetMode::NativeChinese | TargetMode::ImeEnglish | TargetMode::LayoutEnglish
        )
    })
}

fn target_short_label(target: Option<TargetMode>) -> String {
    match target {
        Some(TargetMode::NativeChinese) => "中".into(),
        Some(TargetMode::ImeEnglish) => "英".into(),
        Some(TargetMode::LayoutEnglish) => "ENG".into(),
        _ => "中+E".into(),
    }
}

fn visible_stats(stats: &PersistentStats, day: &str) -> Stats {
    Stats {
        today_auto_switch: if stats.day == day {
            stats.today_auto_switch
        } else {
            0
        },
        total_auto_switch: stats.total_auto_switch,
    }
}
fn rollover(stats: &mut PersistentStats, day: &str) -> bool {
    if stats.day == day {
        return false;
    }
    stats.day = day.into();
    stats.today_auto_switch = 0;
    true
}
fn stats_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("ImeFlow")
        .join("stats.toml")
}
fn persist_to(path: &Path, stats: &PersistentStats) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
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
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
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
        std::env::temp_dir().join(format!(
            "imeflow-stats-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
    fn event(applied: bool, confirmed: bool) -> SwitchEvent {
        SwitchEvent {
            process: "Cursor.exe/terminal".into(),
            target: TargetMode::ImeEnglish,
            applied,
            confirmed,
        }
    }
    #[test]
    fn live_manual_changes_update_icon_without_changing_request_or_count() {
        let path = temporary();
        let bus = FeedbackBus::from_path(path.clone());
        bus.record_switch(event(true, true));
        let revision = bus.observation_revision();
        let stats_file = std::fs::read_to_string(&path).unwrap();
        assert!(bus.record_observation(
            revision,
            LayerObservation::Confident(TargetMode::NativeChinese)
        ));
        assert_eq!(
            bus.tray_icon_mode(Some(TargetMode::ImeEnglish)),
            Some(TargetMode::NativeChinese)
        );
        assert_eq!(bus.tray_mode_label(), "当前·中");
        assert_eq!(bus.ui_snapshot().1.target, Some(TargetMode::ImeEnglish));
        assert_eq!(bus.ui_snapshot().0.total_auto_switch, 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), stats_file);
        assert!(!bus.record_observation(
            revision,
            LayerObservation::Confident(TargetMode::NativeChinese)
        ));
        assert!(bus.record_observation(
            revision,
            LayerObservation::Confident(TargetMode::ImeEnglish)
        ));
        assert_eq!(bus.tray_icon_mode(None), Some(TargetMode::ImeEnglish));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn weak_and_unreadable_live_state_never_claim_confirmation() {
        let bus = FeedbackBus::from_path(temporary());
        bus.record_requested("demo.exe", TargetMode::ImeEnglish);
        let revision = bus.observation_revision();
        bus.record_observation(
            revision,
            LayerObservation::Probable(TargetMode::NativeChinese),
        );
        assert_eq!(bus.tray_icon_mode(None), Some(TargetMode::NativeChinese));
        assert!(bus.tray_mode_label().contains("推测"));
        assert!(bus.tray_mode_label().contains("未确认"));
        bus.record_observation(revision, LayerObservation::Unknown);
        assert_eq!(bus.tray_icon_mode(Some(TargetMode::ImeEnglish)), None);
        assert_eq!(bus.tray_remembered_mode(), None);
        assert_eq!(bus.tray_mode_label(), "当前状态未知");
        assert_eq!(bus.ui_snapshot().0.total_auto_switch, 0);
    }

    #[test]
    fn waiting_and_unconfirmed_actions_do_not_light_the_requested_english_mode() {
        let bus = FeedbackBus::from_path(temporary());
        bus.record_deferred("terminal", TargetMode::ImeEnglish);
        assert_eq!(bus.tray_icon_mode(Some(TargetMode::NativeChinese)), None);
        bus.record_requested("terminal", TargetMode::ImeEnglish);
        assert_eq!(bus.tray_icon_mode(Some(TargetMode::ImeEnglish)), None);
        bus.record_switch(event(true, false));
        assert_eq!(bus.tray_icon_mode(Some(TargetMode::ImeEnglish)), None);
        bus.record_observation(
            bus.observation_revision(),
            LayerObservation::Probable(TargetMode::NativeChinese),
        );
        assert_eq!(bus.tray_icon_mode(None), Some(TargetMode::NativeChinese));
        assert_eq!(bus.ui_snapshot().0.total_auto_switch, 0);
    }

    #[test]
    fn stale_observations_cannot_override_focus_or_a_new_automatic_action() {
        let bus = FeedbackBus::from_path(temporary());
        bus.begin_focus("old", "old.exe");
        let old = bus.observation_revision();
        bus.begin_focus("new", "new.exe");
        let chinese = LayerObservation::Confident(TargetMode::NativeChinese);
        assert!(!bus.record_observation(old, chinese));
        let current = bus.observation_revision();
        bus.record_observation(current, chinese);
        bus.record_requested("new.exe", TargetMode::ImeEnglish);
        assert!(!bus.record_observation(current, chinese));
        assert_eq!(bus.tray_mode_label(), "待确认·英");
        let current = bus.observation_revision();
        bus.record_switch(event(true, false));
        assert!(!bus.record_observation(current, chinese));
    }
    #[test]
    fn confirmed_actions_persist_and_replace_existing_file() {
        let path = temporary();
        let bus = FeedbackBus::from_path(path.clone());
        bus.record_switch(event(true, true));
        bus.record_switch(event(true, true));
        assert_eq!(bus.ui_snapshot().0.today_auto_switch, 2);
        assert_eq!(
            FeedbackBus::from_path(path.clone())
                .ui_snapshot()
                .0
                .total_auto_switch,
            2
        );
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn invalid_unconfirmed_skip_and_passthrough_never_count() {
        let path = temporary();
        let bus = FeedbackBus::from_path(path.clone());
        bus.record_switch(event(false, true));
        bus.record_switch(event(true, false));
        let mut pass = event(true, true);
        pass.target = TargetMode::Passthrough;
        bus.record_switch(pass);
        bus.record_skip_same("test", TargetMode::ImeEnglish, false);
        assert_eq!(bus.last_status().1, None);
        bus.record_passthrough("explorer.exe");
        assert_eq!(bus.ui_snapshot().0.today_auto_switch, 0);
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn new_focus_and_failure_do_not_display_previous_success() {
        let path = temporary();
        let bus = FeedbackBus::from_path(path.clone());
        bus.record_switch(event(true, true));
        assert!(bus.begin_focus("different-scope", "Edge.exe/edit"));
        assert_eq!(bus.ui_snapshot().1.kind, StatusKind::Waiting);
        assert_eq!(bus.last_status().1, None);
        bus.record_switch(event(false, false));
        assert_eq!(bus.ui_snapshot().1.kind, StatusKind::Failed);
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn tray_mode_applied_vs_assumed() {
        let path = temporary();
        let bus = FeedbackBus::from_path(path.clone());
        assert_eq!(bus.tray_mode(), None);
        assert_eq!(bus.tray_icon_mode(None), Some(TargetMode::NativeChinese));
        assert!(bus.begin_focus("chatgpt-edit", "ChatGPT.exe/edit"));
        assert_eq!(
            bus.tray_icon_mode(Some(TargetMode::NativeChinese)),
            Some(TargetMode::NativeChinese)
        );
        bus.record_skip_same("Cursor.exe/edit", TargetMode::NativeChinese, false);
        assert_eq!(bus.tray_mode(), None);
        // 未 Confident 的「已经是中文」按中文显示，不用全蓝待确认。
        assert_eq!(bus.tray_mode_label(), "推测·中（未确认）");
        assert_eq!(bus.tray_icon_mode(None), Some(TargetMode::NativeChinese));
        bus.record_requested("ChatGPT.exe/edit", TargetMode::NativeChinese);
        assert_eq!(bus.tray_mode_label(), "待确认·中");
        assert_eq!(bus.tray_icon_mode(Some(TargetMode::NativeChinese)), None);
        bus.record_passthrough("explorer.exe");
        assert_eq!(
            bus.tray_icon_mode(Some(TargetMode::NativeChinese)),
            Some(TargetMode::NativeChinese)
        );
        bus.record_switch(SwitchEvent {
            process: "msedge.exe/address-bar".into(),
            target: TargetMode::ImeEnglish,
            applied: true,
            confirmed: false,
        });
        assert_eq!(bus.tray_mode(), Some(TargetMode::ImeEnglish));
        assert_eq!(bus.tray_mode_label(), "已请求·英（未确认）");
        bus.record_skip_same("Cursor.exe/terminal", TargetMode::ImeEnglish, true);
        assert_eq!(bus.tray_mode(), Some(TargetMode::ImeEnglish));
        assert_eq!(bus.tray_mode_label(), "英");
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn rollover_and_counter_limits_are_explicit() {
        let mut stats = PersistentStats {
            day: "yesterday".into(),
            today_auto_switch: 7,
            total_auto_switch: 50,
        };
        assert_eq!(visible_stats(&stats, "today").today_auto_switch, 0);
        assert!(rollover(&mut stats, "today"));
        assert_eq!(stats.total_auto_switch, 50);
        assert_eq!(stats.today_auto_switch, 0);
        assert!(!rollover(&mut stats, "today"));
        let path = temporary();
        let bus = FeedbackBus::from_path(path.clone());
        bus.inner.lock().persisted.total_auto_switch = u64::MAX;
        bus.record_switch(event(true, true));
        assert_eq!(bus.ui_snapshot().0.total_auto_switch, u64::MAX);
        let _ = std::fs::remove_file(path);
    }
}
