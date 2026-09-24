//! Wall-clock gap detector: safety net beneath the system-events source for
//! sleeps that produced no suspend event.
//!
//! Owned by agent C. Ground truth:
//! `packages/core/src/runtime/suspendDetector.ts` and `capture-stores.md`
//! §5. MUST use wall-clock time (`SystemTime` / epoch ms), never a
//! monotonic clock — monotonic clocks may pause during sleep, hiding
//! exactly the gap this detects. Sample every 5 s; update the last tick
//! unconditionally BEFORE comparing, so each gap fires the callback at most
//! ONCE (no tokio missed-tick burst after wake). On a gap it logs
//! `warn suspend-detector: "wall-clock gap <N>s"` (N = rounded seconds).
//! The app wiring calls the callback -> forced renegotiation ONLY from
//! `Active`/`Degraded` — that rule lives in the caller, not here.

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::{Condvar, Mutex};

use crate::runtime::logger;

/// Default gap threshold (strictly-greater comparison).
pub const DEFAULT_GAP_THRESHOLD_MS: u64 = 15_000;
/// Sampling interval.
pub const SAMPLE_INTERVAL_MS: u64 = 5_000;

pub struct SuspendDetector {
    inner: Arc<DetectorInner>,
}

struct DetectorInner {
    on_gap_detected: Box<dyn Fn() + Send + Sync>,
    gap_threshold_ms: u64,
    control: Mutex<Control>,
    wake: Condvar,
}

struct Control {
    running: bool,
    /// Bumped on every start; the sampling thread exits when its epoch is
    /// stale, so an interleaved stop()+start() can never leave two loops
    /// alive or wedge stop()'s join.
    epoch: u64,
    join: Option<JoinHandle<()>>,
}

impl SuspendDetector {
    /// Detector with [`DEFAULT_GAP_THRESHOLD_MS`].
    pub fn new(on_gap_detected: Box<dyn Fn() + Send + Sync>) -> Self {
        Self::with_threshold(on_gap_detected, DEFAULT_GAP_THRESHOLD_MS)
    }

    /// Detector with an explicit threshold (tests).
    pub fn with_threshold(
        on_gap_detected: Box<dyn Fn() + Send + Sync>,
        gap_threshold_ms: u64,
    ) -> Self {
        Self {
            inner: Arc::new(DetectorInner {
                on_gap_detected,
                gap_threshold_ms,
                control: Mutex::new(Control {
                    running: false,
                    epoch: 0,
                    join: None,
                }),
                wake: Condvar::new(),
            }),
        }
    }

    /// Idempotent; seeds the last tick with "now" (wall clock).
    pub fn start(&self) {
        let mut control = self.inner.control.lock();
        if control.join.is_some() {
            return;
        }
        control.running = true;
        control.epoch += 1;
        let epoch = control.epoch;
        // Seed BEFORE the thread runs, mirroring `lastTick = Date.now()` in
        // the TS `start()`.
        let seed = epoch_ms_now();
        let inner = Arc::clone(&self.inner);
        control.join = Some(std::thread::spawn(move || sample_loop(inner, seed, epoch)));
    }

    /// Stops the sampling loop promptly (the 2 s bounded shutdown depends on
    /// it); the condvar wakes the thread immediately.
    pub fn stop(&self) {
        let join = {
            let mut control = self.inner.control.lock();
            control.running = false;
            self.inner.wake.notify_all();
            control.join.take()
        };
        if let Some(handle) = join {
            // Never join our own thread (stop() called from inside the gap
            // callback); the loop exits on its own via the cleared flag.
            if handle.thread().id() != std::thread::current().id() {
                let _ = handle.join();
            }
        }
    }
}

fn sample_loop(inner: Arc<DetectorInner>, seed: i64, epoch: u64) {
    let mut last_tick = seed;
    loop {
        // Interruptible 5 s wait; a plain per-iteration delay (not a
        // fixed-rate catch-up timer) so a long sleep cannot fire a burst of
        // callbacks after wake.
        {
            let mut control = inner.control.lock();
            let deadline = Instant::now() + Duration::from_millis(SAMPLE_INTERVAL_MS);
            loop {
                if !control.running || control.epoch != epoch {
                    return;
                }
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                let _ = inner.wake.wait_for(&mut control, deadline - now);
            }
            if !control.running || control.epoch != epoch {
                return;
            }
        }
        let now = epoch_ms_now();
        let gap = now - last_tick;
        // Unconditionally update BEFORE the comparison — a single callback
        // per detected gap, no re-fire.
        last_tick = now;
        if gap_exceeds(gap, inner.gap_threshold_ms) {
            logger::warn("suspend-detector", &gap_log_message(gap));
            (inner.on_gap_detected)();
        }
    }
}

/// Wall-clock epoch milliseconds (`Date.now()` equivalent). A clock before
/// the epoch yields a negative value (practically impossible on Windows).
fn epoch_ms_now() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_millis() as i64,
        Err(err) => -(err.duration().as_millis() as i64),
    }
}

/// Strictly-greater comparison (`gap > gapThresholdMs`); a clock stepped
/// backwards yields a negative gap and never fires.
fn gap_exceeds(gap_ms: i64, threshold_ms: u64) -> bool {
    gap_ms > threshold_ms as i64
}

/// `wall-clock gap <N>s` with `N = Math.round(gap / 1000)`. For positive
/// gaps JS `Math.round` (half toward +inf) and `f64::round` (half away from
/// zero) agree.
fn gap_log_message(gap_ms: i64) -> String {
    format!(
        "wall-clock gap {}s",
        (gap_ms as f64 / 1000.0).round() as i64
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gap_comparison_is_strictly_greater() {
        assert!(!gap_exceeds(14_999, DEFAULT_GAP_THRESHOLD_MS));
        assert!(!gap_exceeds(15_000, DEFAULT_GAP_THRESHOLD_MS));
        assert!(gap_exceeds(15_001, DEFAULT_GAP_THRESHOLD_MS));
    }

    #[test]
    fn negative_gap_never_fires() {
        // Clock stepped backwards: now < lastTick.
        assert!(!gap_exceeds(-60_000, DEFAULT_GAP_THRESHOLD_MS));
        assert!(!gap_exceeds(0, 0));
        assert!(gap_exceeds(1, 0));
    }

    #[test]
    fn gap_message_rounds_like_math_round() {
        assert_eq!(gap_log_message(62_300), "wall-clock gap 62s");
        assert_eq!(gap_log_message(15_500), "wall-clock gap 16s");
        assert_eq!(gap_log_message(15_499), "wall-clock gap 15s");
        assert_eq!(gap_log_message(500), "wall-clock gap 1s");
    }
}
