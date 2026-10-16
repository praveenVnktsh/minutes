//! Logs that survive a launch from Finder, where stderr goes nowhere.
//!
//! [`init`] runs first thing in `main`: it installs a logger that writes to
//! stderr (filtered by `RUST_LOG`, as before) and to `minutes.log` in the
//! app's log directory, and a panic hook that appends the panic message,
//! location, thread and a backtrace to `panic.log` there before handing over
//! to the default hook. A panic inside the macOS launch callback aborts the
//! process (it cannot unwind through Objective-C), so the hook is the only
//! record of what went wrong.
//!
//! The directory mirrors Tauri's `app_log_dir()` (`~/Library/Logs/<identifier>`
//! on macOS, `<local data dir>/<identifier>/logs` elsewhere), worked out with
//! `dirs` because the panic hook has to exist before the Tauri app does. Setup
//! confirms it against the real `app_log_dir()` via [`use_log_dir`].

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

const LOG_FILE: &str = "minutes.log";
const PANIC_FILE: &str = "panic.log";
/// `minutes.log` rolls over to `minutes.log.1` past this size, so the two
/// files hold at most twice this much.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

static LOG_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);
/// The open `minutes.log`. Locked with `try_lock` on the logging and panic
/// paths, so a panic while it is held drops a line rather than deadlocking.
static LOG_FILE_HANDLE: Mutex<Option<LogFile>> = Mutex::new(None);

/// The bundle identifier from tauri.conf.json, which names the log directory.
fn app_identifier() -> Option<String> {
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../tauri.conf.json")).ok()?;
    config.get("identifier")?.as_str().map(str::to_string)
}

/// Where Tauri's `app_log_dir()` points for `identifier`.
fn default_log_dir(identifier: &str) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        dirs::home_dir().map(|home| home.join("Library/Logs").join(identifier))
    }
    #[cfg(not(target_os = "macos"))]
    {
        dirs::data_local_dir().map(|dir| dir.join(identifier).join("logs"))
    }
}

/// The directory logs and panic reports go to, if one could be resolved.
pub fn log_dir() -> Option<PathBuf> {
    LOG_DIR
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Install the panic hook and the stderr + file logger. Call once, first.
pub fn init() {
    let dir = app_identifier().and_then(|identifier| default_log_dir(&identifier));
    *LOG_DIR.write().unwrap_or_else(|p| p.into_inner()) = dir.clone();

    install_panic_hook();

    let stderr =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).build();
    *LOG_FILE_HANDLE.lock().unwrap_or_else(|p| p.into_inner()) =
        dir.as_deref().and_then(|dir| LogFile::open(dir).ok());
    let max_level = max_level_for(stderr.filter());
    if log::set_boxed_logger(Box::new(AppLogger { stderr })).is_ok() {
        log::set_max_level(max_level);
    }
    match dir {
        Some(dir) => log::info!("Writing logs to {}", dir.display()),
        None => log::warn!("No log directory could be resolved; logging to stderr only"),
    }
}

/// The global level the `log` macros filter at: what RUST_LOG asks of
/// stderr, and never below info, which the file always keeps. A fixed Info
/// silenced RUST_LOG=debug (and perf_debug!) everywhere, because the macros
/// drop records above the global level before any logger sees them.
fn max_level_for(stderr_filter: log::LevelFilter) -> log::LevelFilter {
    stderr_filter.max(log::LevelFilter::Info)
}

/// Point logs and panic reports at `dir` (Tauri's resolved `app_log_dir()`)
/// when it differs from the one [`init`] worked out.
pub fn use_log_dir(dir: PathBuf) {
    if log_dir().as_deref() == Some(dir.as_path()) {
        return;
    }
    log::warn!("Moving logs to Tauri's log directory {}", dir.display());
    *LOG_DIR.write().unwrap_or_else(|p| p.into_inner()) = Some(dir.clone());
    *LOG_FILE_HANDLE.lock().unwrap_or_else(|p| p.into_inner()) = LogFile::open(&dir).ok();
}

struct LogFile {
    path: PathBuf,
    file: File,
    written: u64,
}

impl LogFile {
    fn open(dir: &Path) -> std::io::Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join(LOG_FILE);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        let mut log = LogFile {
            path,
            file,
            written,
        };
        if log.written >= MAX_LOG_BYTES {
            log.rotate()?;
        }
        Ok(log)
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        let rolled = self.path.with_extension("log.1");
        let _ = fs::remove_file(&rolled);
        fs::rename(&self.path, &rolled)?;
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.written = 0;
        Ok(())
    }

    fn write_line(&mut self, line: &str) {
        if self.written >= MAX_LOG_BYTES && self.rotate().is_err() {
            return;
        }
        if self.file.write_all(line.as_bytes()).is_ok() {
            self.written += line.len() as u64;
        }
    }
}

struct AppLogger {
    stderr: env_logger::Logger,
}

/// The file keeps info and above, plus this app's debug lines in debug mode;
/// dependencies' debug output would bury everything else.
fn file_accepts(metadata: &log::Metadata) -> bool {
    metadata.level() <= log::Level::Info
        || (metadata.level() <= log::max_level() && metadata.target().starts_with("app_lib"))
}

impl log::Log for AppLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.stderr.enabled(metadata) || file_accepts(metadata)
    }

    fn log(&self, record: &log::Record) {
        if self.stderr.matches(record) {
            self.stderr.log(record);
        }
        if !file_accepts(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {:<5} {}: {}\n",
            chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z"),
            record.level(),
            record.target(),
            record.args()
        );
        if let Ok(mut file) = LOG_FILE_HANDLE.try_lock() {
            if let Some(file) = file.as_mut() {
                file.write_line(&line);
            }
        }
    }

    fn flush(&self) {
        self.stderr.flush();
        if let Ok(mut file) = LOG_FILE_HANDLE.try_lock() {
            if let Some(file) = file.as_mut() {
                let _ = file.file.flush();
            }
        }
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic payload>")
}

/// The text written to `panic.log` for one panic.
fn panic_report(info: &std::panic::PanicHookInfo<'_>, backtrace: &str) -> String {
    let thread = std::thread::current();
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "<unknown location>".to_string());
    format!(
        "==== {} panic (v{}) on thread '{}' at {}\n{}\nbacktrace:\n{}\n\n",
        chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z"),
        env!("CARGO_PKG_VERSION"),
        thread.name().unwrap_or("<unnamed>"),
        location,
        panic_message(info.payload()),
        backtrace
    )
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        let report = panic_report(info, &backtrace);
        if let Some(dir) = log_dir() {
            let _ = fs::create_dir_all(&dir);
            if let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(PANIC_FILE))
            {
                let _ = file.write_all(report.as_bytes());
                let _ = file.sync_all();
            }
        }
        // Also in the main log, next to what led up to it (skipped if the
        // logger itself panicked while holding its file).
        if let Ok(mut file) = LOG_FILE_HANDLE.try_lock() {
            if let Some(file) = file.as_mut() {
                file.write_line(&report);
                let _ = file.file.flush();
            }
        }
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_log_debug_raises_the_global_level() {
        assert_eq!(
            max_level_for(log::LevelFilter::Debug),
            log::LevelFilter::Debug
        );
        assert_eq!(
            max_level_for(log::LevelFilter::Trace),
            log::LevelFilter::Trace
        );
        assert_eq!(
            max_level_for(log::LevelFilter::Warn),
            log::LevelFilter::Info
        );
    }

    #[test]
    fn identifier_comes_from_the_tauri_config() {
        assert_eq!(app_identifier().as_deref(), Some("com.meetily.ai"));
    }

    #[test]
    fn log_dir_follows_tauri_convention() {
        let dir = default_log_dir("com.example.app").expect("a home directory");
        #[cfg(target_os = "macos")]
        assert!(dir.ends_with("Library/Logs/com.example.app"));
        #[cfg(not(target_os = "macos"))]
        assert!(dir.ends_with("com.example.app/logs"));
    }

    #[test]
    fn log_file_rolls_over_past_the_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = LogFile::open(dir.path()).unwrap();
        log.written = MAX_LOG_BYTES;
        log.write_line("after rollover\n");
        assert_eq!(
            fs::read_to_string(dir.path().join(LOG_FILE)).unwrap(),
            "after rollover\n"
        );
        assert!(dir.path().join("minutes.log.1").exists());
    }
}
