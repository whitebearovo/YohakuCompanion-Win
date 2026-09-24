//! Runtime helpers: content-redacting logger and the wall-clock suspend
//! detector.
//!
//! Scaffold-owned mod file. Ownership: agent S owns logger; agent C owns
//! suspend_detector.

pub mod logger;
pub mod suspend_detector;

pub use logger::{log, recent_log_lines, set_log_level, LogLevel, RING_LIMIT};
pub use suspend_detector::SuspendDetector;
