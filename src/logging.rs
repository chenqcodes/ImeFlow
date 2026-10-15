//! Immutable session logs; only the owning daemon updates latest.log.
use parking_lot::Mutex;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tracing_subscriber::{fmt::MakeWriter, EnvFilter};
use windows::Win32::System::SystemInformation::GetLocalTime;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    Daemon,
    Diagnostic,
}

pub fn log_directory() -> PathBuf {
    std::env::var_os("IMEFLOW_LOG_DIR")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir)
                .join("ImeFlow")
                .join("logs")
        })
}

pub fn init(debug_mode: bool, kind: SessionKind) -> Result<PathBuf, String> {
    let rust_log = nonempty_env("RUST_LOG");
    let log_level = nonempty_env("IMEFLOW_LOG_LEVEL");
    let env_file = if rust_log.is_none() && log_level.is_none() && !debug_mode {
        find_env_file(
            &std::env::current_exe().map_err(|e| e.to_string())?,
            cfg!(debug_assertions).then(|| Path::new(env!("CARGO_MANIFEST_DIR"))),
        )
    } else {
        None
    };
    let file_level = env_file
        .as_deref()
        .map(read_env_level)
        .transpose()?
        .flatten();
    let (filter, filter_text, source) = select_filter(
        debug_mode,
        rust_log.as_deref(),
        log_level.as_deref(),
        file_level.as_deref(),
    )?;
    let (mut files, path) =
        open_session(&log_directory(), kind).map_err(|e| format!("open session log: {e}"))?;
    writeln!(
        files,
        "=== ImeFlow {} filter={filter_text} source={source} log={} ===",
        local_stamp(),
        path.display()
    )
    .map_err(|e| e.to_string())?;
    files.flush().map_err(|e| e.to_string())?;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_target(false)
        .with_writer(FileLogWriter(Arc::new(Mutex::new(files))))
        .try_init()
        .map_err(|e| e.to_string())?;
    tracing::info!(filter = %filter_text, source, config = ?env_file, path = %path.display(), "session log ready");
    Ok(path)
}

fn nonempty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn find_env_file(exe: &Path, development_root: Option<&Path>) -> Option<PathBuf> {
    let adjacent = exe.parent()?.join(".env");
    if adjacent.is_file() {
        return Some(adjacent);
    }
    // Release builds must never inherit a repository .env through their working directory.
    development_root
        .map(|root| root.join(".env"))
        .filter(|path| path.is_file())
}

fn read_env_level(path: &Path) -> Result<Option<String>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("read log config {}: {e}", path.display()))?;
    parse_env_level(&text).map_err(|e| format!("log config {}: {e}", path.display()))
}

fn parse_env_level(text: &str) -> Result<Option<String>, String> {
    let mut level = None;
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "IMEFLOW_LOG_LEVEL" {
            continue;
        }
        if level.is_some() {
            return Err("duplicate IMEFLOW_LOG_LEVEL".into());
        }
        let value = value.split('#').next().unwrap_or_default().trim();
        let value = if value.starts_with(['\'', '"']) {
            let quote = value.as_bytes()[0] as char;
            value
                .strip_prefix(quote)
                .and_then(|v| v.strip_suffix(quote))
                .ok_or("unclosed IMEFLOW_LOG_LEVEL quote")?
        } else {
            value
        };
        let parsed: tracing::level_filters::LevelFilter = value
            .parse()
            .map_err(|_| "IMEFLOW_LOG_LEVEL must be off/error/warn/info/debug/trace")?;
        level = Some(parsed.to_string().to_ascii_lowercase());
    }
    Ok(level)
}

fn select_filter(
    debug_mode: bool,
    rust_log: Option<&str>,
    log_level: Option<&str>,
    file_level: Option<&str>,
) -> Result<(EnvFilter, String, &'static str), String> {
    let (value, source) = if let Some(value) = rust_log {
        (value, "RUST_LOG")
    } else if let Some(value) = log_level {
        (value, "IMEFLOW_LOG_LEVEL")
    } else if debug_mode {
        ("debug", "--debug")
    } else if let Some(value) = file_level {
        (value, ".env")
    } else {
        ("warn", "default")
    };
    if source != "RUST_LOG" {
        value
            .parse::<tracing::level_filters::LevelFilter>()
            .map_err(|_| format!("{source}: expected off/error/warn/info/debug/trace"))?;
    }
    let filter =
        EnvFilter::try_new(value).map_err(|_| format!("invalid log filter in {source}"))?;
    Ok((filter, value.to_string(), source))
}

static SESSION_ID: AtomicU64 = AtomicU64::new(0);
fn open_session(dir: &Path, kind: SessionKind) -> std::io::Result<(LogFiles, PathBuf)> {
    std::fs::create_dir_all(dir)?;
    let prefix = if kind == SessionKind::Daemon {
        "session"
    } else {
        "diagnostic"
    };
    let stamp = local_stamp().replace([' ', ':'], "-");
    let path = dir.join(format!(
        "{prefix}-{stamp}-{}-{}.log",
        std::process::id(),
        SESSION_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let session = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)?;
    let latest = if kind == SessionKind::Daemon {
        Some(
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(dir.join("latest.log"))?,
        )
    } else {
        None
    };
    Ok((LogFiles { session, latest }, path))
}
fn local_stamp() -> String {
    let t = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}
struct LogFiles {
    session: File,
    latest: Option<File>,
}
impl Write for LogFiles {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.session.write_all(buf)?;
        if let Some(latest) = &mut self.latest {
            latest.write_all(buf)?;
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.session.flush()?;
        if let Some(latest) = &mut self.latest {
            latest.flush()?;
        }
        Ok(())
    }
}
struct FileLogWriter(Arc<Mutex<LogFiles>>);
impl<'a> MakeWriter<'a> for FileLogWriter {
    type Writer = FileLogGuard;
    fn make_writer(&'a self) -> Self::Writer {
        FileLogGuard(self.0.clone())
    }
}
struct FileLogGuard(Arc<Mutex<LogFiles>>);
impl Write for FileLogGuard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().flush()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_precedence_and_quiet_default_are_explicit() {
        assert_eq!(select_filter(false, None, None, None).unwrap().1, "warn");
        assert_eq!(
            select_filter(false, None, None, Some("debug")).unwrap().2,
            ".env"
        );
        assert_eq!(
            select_filter(true, None, None, Some("warn")).unwrap().2,
            "--debug"
        );
        assert_eq!(
            select_filter(true, None, Some("error"), Some("debug"))
                .unwrap()
                .1,
            "error"
        );
        assert_eq!(
            select_filter(true, Some("imeflow=trace"), Some("warn"), Some("debug"))
                .unwrap()
                .2,
            "RUST_LOG"
        );
        assert!(select_filter(false, None, Some("typo"), None).is_err());
        assert_eq!(
            parse_env_level(
                "# local\nexport IMEFLOW_LOG_LEVEL='debug' # comment\nUNRELATED=secret"
            )
            .unwrap(),
            Some("debug".into())
        );
        assert!(parse_env_level("IMEFLOW_LOG_LEVEL=debug\nIMEFLOW_LOG_LEVEL=warn").is_err());
        assert!(parse_env_level("IMEFLOW_LOG_LEVEL=typo").is_err());
    }

    #[test]
    fn release_config_lookup_does_not_inherit_development_env() {
        let dir = std::env::temp_dir().join(format!("imeflow-env-test-{}", std::process::id()));
        let dev = dir.join("repo");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(dev.join(".env"), "IMEFLOW_LOG_LEVEL=debug").unwrap();
        let exe = bin.join("imeflow.exe");
        assert_eq!(find_env_file(&exe, Some(&dev)), Some(dev.join(".env")));
        assert_eq!(find_env_file(&exe, None), None);
        std::fs::write(bin.join(".env"), "IMEFLOW_LOG_LEVEL=warn").unwrap();
        assert_eq!(find_env_file(&exe, Some(&dev)), Some(bin.join(".env")));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn diagnostic_does_not_truncate_daemon_and_sessions_survive_restart() {
        let dir = std::env::temp_dir().join(format!(
            "imeflow-log-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let (mut daemon, first) = open_session(&dir, SessionKind::Daemon).unwrap();
        daemon.write_all(b"original daemon").unwrap();
        daemon.flush().unwrap();
        let (mut diagnostic, probe) = open_session(&dir, SessionKind::Diagnostic).unwrap();
        diagnostic.write_all(b"asset check").unwrap();
        diagnostic.flush().unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("latest.log")).unwrap(),
            "original daemon"
        );
        assert_ne!(first, probe);
        drop(daemon);
        drop(diagnostic);
        let (mut next, second) = open_session(&dir, SessionKind::Daemon).unwrap();
        next.write_all(b"next daemon").unwrap();
        drop(next);
        assert_ne!(first, second);
        assert_eq!(std::fs::read_to_string(first).unwrap(), "original daemon");
        assert_eq!(std::fs::read_to_string(second).unwrap(), "next daemon");
        for entry in std::fs::read_dir(&dir).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).unwrap();
        }
        std::fs::remove_dir(dir).unwrap();
    }
}
