//! Persistent crash diagnostics: panics (with a backtrace and the last log lines) and exit codes
//! under `<app data>/crash/`.
//!
//! The app logs to the console and keeps a tail in memory (`flight::applog`), so a process that dies
//! without unwinding leaves nothing behind. These files survive the process: a panic hook writes one
//! file per panic, and `RunEvent::ExitRequested` / `Exit` append the exit code it was given.

use std::backtrace::Backtrace;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const CRASH_DIR: &str = "crash";
/// `exits.log` is rolled to `exits.log.1` once it grows past this.
const MAX_EXIT_LOG_BYTES: u64 = 256 * 1024;

/// Install the panic hook. `data_dir` is the app data folder; the files go in its `crash/` child.
/// The previous hook still runs, so the console output does not change.
pub fn install(data_dir: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let report = report(info);
        match write_report(&data_dir, "panic", &report) {
            Some(path) => eprintln!("Puppet Master panic written to {}", path.display()),
            None => eprintln!("Puppet Master panic: the crash file could not be written"),
        }
        previous(info);
    }));
}

/// Append the exit code and the reason the process is leaving. Best effort: a full disk must not
/// change how the app exits.
pub fn record_exit(data_dir: &Path, code: Option<i32>, reason: &str) {
    let dir = data_dir.join(CRASH_DIR);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("exits.log");
    rotate(&path);
    let code = code.map(|c| c.to_string()).unwrap_or_else(|| "none".to_string());
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{} pid={} code={} reason={}", now_ms(), std::process::id(), code, reason);
    }
}

fn rotate(path: &Path) {
    let too_big = std::fs::metadata(path).map(|m| m.len() > MAX_EXIT_LOG_BYTES).unwrap_or(false);
    if too_big {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
}

fn report(info: &std::panic::PanicHookInfo<'_>) -> String {
    let when = now_ms();
    let thread = std::thread::current();
    let thread = thread.name().unwrap_or("unnamed");
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "unknown".to_string());
    let message = payload_message(info);
    let mut text = format!(
        "Puppet Master panic\nwhen_ms: {when}\npid: {}\nthread: {thread}\nlocation: {location}\nmessage: {message}\n\nbacktrace:\n{}",
        std::process::id(),
        Backtrace::force_capture()
    );
    let tail = crate::flight::applog::tail();
    if !tail.is_empty() {
        text.push_str("\n--- last log lines ---\n");
        for line in tail {
            text.push_str(&line);
            text.push('\n');
        }
    }
    text
}

fn payload_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "(not a string)".to_string()
    }
}

fn write_report(data_dir: &Path, kind: &str, text: &str) -> Option<PathBuf> {
    let dir = data_dir.join(CRASH_DIR);
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{kind}-{}.log", now_ms()));
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok()?;
    file.write_all(text.as_bytes()).ok()?;
    file.write_all(b"\n").ok()?;
    Some(path)
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pm-crash-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_report_is_written_under_crash() {
        let dir = temp("report");
        let path = write_report(&dir, "panic", "boom").unwrap();
        assert_eq!(path.parent(), Some(dir.join(CRASH_DIR).as_path()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "boom\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exit_codes_are_appended_with_the_reason() {
        let dir = temp("exit");
        record_exit(&dir, Some(101), "ExitRequested");
        record_exit(&dir, None, "Exit");
        let text = std::fs::read_to_string(dir.join(CRASH_DIR).join("exits.log")).unwrap();
        assert!(text.contains("code=101 reason=ExitRequested"), "{text}");
        assert!(text.contains("code=none reason=Exit"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_full_exit_log_is_rolled() {
        let dir = temp("rotate");
        let crash = dir.join(CRASH_DIR);
        std::fs::create_dir_all(&crash).unwrap();
        std::fs::write(crash.join("exits.log"), vec![b'x'; MAX_EXIT_LOG_BYTES as usize + 1]).unwrap();
        record_exit(&dir, Some(0), "Exit");
        assert!(crash.join("exits.log.1").is_file());
        assert!(std::fs::metadata(crash.join("exits.log")).unwrap().len() < MAX_EXIT_LOG_BYTES);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
