//! Content-redacting logger. Log lines never include tokens, window titles,
//! media text, exe paths, or any raw capture values — only fixed messages,
//! error names/codes and counts. This is a hard privacy boundary, not a
//! formatting choice.
//!
//! Owned by agent S. Ground truth: `packages/core/src/runtime/logger.ts`
//! and `capture-stores.md` §6. Line format (exact):
//! `<ISO-8601 UTC, 3 fraction digits, Z> [<level>] <scope>: <message>`.
//! Filtering below the minimum level drops the line ENTIRELY (neither
//! console nor ring buffer). error/warn -> stderr; info/debug -> stdout.
//! Existing scopes include: foreground, displayName, media, system-events,
//! suspend-detector, config, credentials, service, main.
//!
//! REDACTION IS ENFORCED BY API SHAPE: `log` takes a scope tag and a message
//! string; this module never formats capture values or secrets on its own,
//! and callers are contract-bound to pass fixed strings (plus at most error
//! names/codes and counts — never titles, media text, paths, or tokens).

use std::collections::VecDeque;

use once_cell::sync::Lazy;
use parking_lot::Mutex;

/// Levels order debug(10) < info(20) < warn(30) < error(40); default
/// minimum is `Info`. (`derive(Ord)` on the variant order reproduces the TS
/// `LEVEL_ORDER` comparison.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    /// The lowercase level word used in the line format.
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

/// Ring buffer capacity (FIFO eviction of the oldest line).
pub const RING_LIMIT: usize = 200;

/// Minimum level + ring buffer. A standalone struct (rather than bare
/// statics) so the filtering/eviction rules are unit-testable on a local
/// instance without racing other tests through the process-global state.
struct LoggerState {
    minimum: LogLevel,
    ring: VecDeque<String>,
}

impl LoggerState {
    fn new() -> Self {
        Self {
            minimum: LogLevel::Info,
            ring: VecDeque::with_capacity(RING_LIMIT),
        }
    }

    /// Filter + format + buffer one line. Returns the formatted line when it
    /// passed the level filter (the caller prints it), `None` when dropped —
    /// a dropped line reaches neither the console nor the ring buffer.
    fn append(&mut self, level: LogLevel, scope: &str, message: &str) -> Option<String> {
        if level < self.minimum {
            return None;
        }
        let line = format_line(level, scope, message, &now_iso());
        self.ring.push_back(line.clone());
        if self.ring.len() > RING_LIMIT {
            self.ring.pop_front();
        }
        Some(line)
    }
}

static STATE: Lazy<Mutex<LoggerState>> = Lazy::new(|| Mutex::new(LoggerState::new()));

/// `${timestamp} [${level}] ${scope}: ${message}` — byte-for-byte the TS
/// template literal.
fn format_line(level: LogLevel, scope: &str, message: &str, timestamp: &str) -> String {
    format!("{timestamp} [{}] {scope}: {message}", level.as_str())
}

/// UTC ISO-8601 with exactly 3 fractional digits and a `Z` suffix, matching
/// JS `Date.prototype.toISOString()` (e.g. `2026-09-23T12:34:56.789Z`).
fn now_iso() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// Change the global minimum level at runtime.
pub fn set_log_level(level: LogLevel) {
    STATE.lock().minimum = level;
}

/// Shallow copy of the ring buffer contents, oldest first (Status page).
pub fn recent_log_lines() -> Vec<String> {
    STATE.lock().ring.iter().cloned().collect()
}

/// Format, buffer and print one line (see module docs for the format).
pub fn log(level: LogLevel, scope: &str, message: &str) {
    let line = match STATE.lock().append(level, scope, message) {
        Some(line) => line,
        None => return,
    };
    // Printed OUTSIDE the state lock. Stream split per logger.ts:
    // error/warn -> stderr (console.error), info/debug -> stdout (console.log).
    if matches!(level, LogLevel::Warn | LogLevel::Error) {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
}

pub fn debug(scope: &str, message: &str) {
    log(LogLevel::Debug, scope, message);
}

pub fn info(scope: &str, message: &str) {
    log(LogLevel::Info, scope, message);
}

pub fn warn(scope: &str, message: &str) {
    log(LogLevel::Warn, scope, message);
}

pub fn error(scope: &str, message: &str) {
    log(LogLevel::Error, scope, message);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec §11 pinned vector:
    /// `2026-01-02T03:04:05.678Z [info] foreground: watcher started`.
    #[test]
    fn format_line_matches_ts_template() {
        assert_eq!(
            format_line(
                LogLevel::Info,
                "foreground",
                "watcher started",
                "2026-01-02T03:04:05.678Z"
            ),
            "2026-01-02T03:04:05.678Z [info] foreground: watcher started"
        );
        assert_eq!(
            format_line(
                LogLevel::Error,
                "config",
                "boom",
                "2026-01-02T03:04:05.678Z"
            ),
            "2026-01-02T03:04:05.678Z [error] config: boom"
        );
        assert_eq!(
            format_line(LogLevel::Warn, "media", "x", "2026-01-02T03:04:05.678Z"),
            "2026-01-02T03:04:05.678Z [warn] media: x"
        );
        assert_eq!(
            format_line(
                LogLevel::Debug,
                "displayName",
                "y",
                "2026-01-02T03:04:05.678Z"
            ),
            "2026-01-02T03:04:05.678Z [debug] displayName: y"
        );
    }

    #[test]
    fn level_ordering_matches_numeric_order() {
        // debug(10) < info(20) < warn(30) < error(40)
        assert!(LogLevel::Debug < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Error);
    }

    /// `YYYY-MM-DDTHH:mm:ss.sssZ` — 24 chars, 3 fraction digits, `Z`.
    #[test]
    fn now_iso_matches_to_iso_string_shape() {
        let value = now_iso();
        let bytes = value.as_bytes();
        assert_eq!(bytes.len(), 24, "{value}");
        for (index, byte) in bytes.iter().enumerate() {
            match index {
                4 | 7 => assert_eq!(*byte, b'-', "{value}"),
                10 => assert_eq!(*byte, b'T', "{value}"),
                13 | 16 => assert_eq!(*byte, b':', "{value}"),
                19 => assert_eq!(*byte, b'.', "{value}"),
                23 => assert_eq!(*byte, b'Z', "{value}"),
                _ => assert!(byte.is_ascii_digit(), "{value}"),
            }
        }
    }

    #[test]
    fn below_minimum_is_dropped_entirely() {
        let mut state = LoggerState::new(); // default minimum: Info
        assert!(state.append(LogLevel::Debug, "scope", "dropped").is_none());
        assert!(
            state.ring.is_empty(),
            "a filtered line must not reach the ring"
        );
        assert!(state.append(LogLevel::Info, "scope", "kept").is_some());
        assert_eq!(state.ring.len(), 1);
    }

    #[test]
    fn lowering_minimum_enables_debug() {
        let mut state = LoggerState::new();
        state.minimum = LogLevel::Debug;
        let line = state
            .append(LogLevel::Debug, "scope", "now visible")
            .unwrap();
        assert!(line.ends_with(" [debug] scope: now visible"));
        assert_eq!(state.ring.len(), 1);
    }

    #[test]
    fn raising_minimum_drops_info_and_warn() {
        let mut state = LoggerState::new();
        state.minimum = LogLevel::Error;
        assert!(state.append(LogLevel::Warn, "scope", "dropped").is_none());
        assert!(state.append(LogLevel::Info, "scope", "dropped").is_none());
        assert!(state.append(LogLevel::Error, "scope", "kept").is_some());
        assert_eq!(state.ring.len(), 1);
    }

    #[test]
    fn ring_evicts_oldest_beyond_limit() {
        let mut state = LoggerState::new();
        for index in 0..(RING_LIMIT + 5) {
            state
                .append(LogLevel::Info, "ring", &format!("m-{index}"))
                .unwrap();
        }
        assert_eq!(state.ring.len(), RING_LIMIT);
        // Pushed m-0..=m-204; the five oldest were evicted, so the ring now
        // starts at m-5 and ends at m-204, oldest first.
        assert!(state.ring.front().unwrap().ends_with(" [info] ring: m-5"));
        assert!(state
            .ring
            .back()
            .unwrap()
            .ends_with(&format!(" [info] ring: m-{}", RING_LIMIT + 4)));
    }

    /// Global API smoke test. Other tests in the crate log concurrently, so
    /// only assert on a unique marker rather than exact ring contents.
    #[test]
    fn global_log_reaches_recent_log_lines() {
        let marker = format!("logger-smoke-{}", uuid::Uuid::new_v4());
        info("test-scope", &marker);
        let lines = recent_log_lines();
        let found = lines
            .iter()
            .find(|line| line.contains(&marker))
            .expect("marker line present in recent_log_lines()");
        assert!(found.contains(" [info] test-scope: "));
        assert!(found.ends_with(&marker));
    }

    #[test]
    fn global_debug_is_filtered_by_default_minimum() {
        // Default minimum is Info; other tests never lower it (set_log_level
        // is exercised only through local LoggerState instances above).
        let marker = format!("logger-debug-{}", uuid::Uuid::new_v4());
        debug("test-scope", &marker);
        assert!(
            recent_log_lines()
                .iter()
                .all(|line| !line.contains(&marker)),
            "debug below the minimum level must not reach the ring"
        );
    }
}
