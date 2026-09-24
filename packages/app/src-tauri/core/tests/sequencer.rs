//! Ported from `packages/core/test/companion/sequencer.test.ts` (spec §11.2).

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use yohaku_core::protocol::sequencer::{CompanionSequencer, SequenceBacking, SequencerError};
use yohaku_core::protocol::wire::MAXIMUM_SAFE_WIRE_INTEGER;

const DEVICE: &str = "3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b";

#[derive(Debug, Clone, PartialEq, Eq)]
enum LogEntry {
    Load,
    Store(u64),
}

#[derive(Default)]
struct MemoryInner {
    values: HashMap<String, i64>,
    log: Vec<LogEntry>,
    fail_next_store: bool,
}

/// In-memory `SequencePersistence` mirroring the vitest fixture: an op log
/// plus a `fail_next_store` flag that makes exactly the next store fail with
/// "disk full" without recording or writing.
#[derive(Default)]
struct MemoryBacking {
    inner: Mutex<MemoryInner>,
}

impl MemoryBacking {
    fn set_value(&self, device_id: &str, value: i64) {
        self.inner
            .lock()
            .unwrap()
            .values
            .insert(device_id.to_string(), value);
    }

    fn value(&self, device_id: &str) -> Option<i64> {
        self.inner.lock().unwrap().values.get(device_id).copied()
    }

    fn last_log(&self) -> Option<LogEntry> {
        self.inner.lock().unwrap().log.last().cloned()
    }

    fn fail_next_store(&self) {
        self.inner.lock().unwrap().fail_next_store = true;
    }
}

#[async_trait]
impl SequenceBacking for MemoryBacking {
    async fn load(&self, device_id: &str) -> Option<i64> {
        let mut inner = self.inner.lock().unwrap();
        inner.log.push(LogEntry::Load);
        inner.values.get(device_id).copied()
    }

    async fn store(&self, device_id: &str, next: u64) -> io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if inner.fail_next_store {
            inner.fail_next_store = false;
            return Err(io::Error::other("disk full"));
        }
        inner.log.push(LogEntry::Store(next));
        inner.values.insert(device_id.to_string(), next as i64);
        Ok(())
    }
}

fn sequencer(backing: &Arc<MemoryBacking>, base: u64) -> CompanionSequencer {
    CompanionSequencer::new(backing.clone(), DEVICE, base)
}

#[tokio::test]
async fn starts_from_pairing_next_sequence_and_persists_before_returning() {
    let backing = Arc::new(MemoryBacking::default());
    let s = sequencer(&backing, 5);
    let reserved = s.reserve().await.unwrap();
    assert_eq!(reserved, 5);
    assert_eq!(backing.value(DEVICE), Some(6));
    // store happened before reserve resolved
    assert_eq!(backing.last_log(), Some(LogEntry::Store(6)));
}

#[tokio::test]
async fn is_monotonic_across_reserves() {
    let backing = Arc::new(MemoryBacking::default());
    let s = sequencer(&backing, 0);
    assert_eq!(s.reserve().await.unwrap(), 0);
    assert_eq!(s.reserve().await.unwrap(), 1);
    assert_eq!(s.reserve().await.unwrap(), 2);
}

#[tokio::test]
async fn a_crash_after_persist_produces_a_legal_gap_never_reuse() {
    let backing = Arc::new(MemoryBacking::default());
    let s1 = sequencer(&backing, 0);
    // 0 reserved, next=1 persisted; pretend crash before send
    s1.reserve().await.unwrap();
    let s2 = sequencer(&backing, 0); // restart
    assert_eq!(s2.reserve().await.unwrap(), 1); // gap at 0 is fine; no reuse
}

#[tokio::test]
async fn failed_persistence_fails_the_reserve_no_sequence_handed_out() {
    let backing = Arc::new(MemoryBacking::default());
    backing.fail_next_store();
    let s = sequencer(&backing, 0);
    let err = s.reserve().await.unwrap_err();
    assert_eq!(err.to_string(), "disk full");
    // next reserve still starts at 0 — nothing was consumed
    assert_eq!(s.reserve().await.unwrap(), 0);
}

#[tokio::test]
async fn reconcile_advances_next_to_accepted_plus_1_never_backwards() {
    let backing = Arc::new(MemoryBacking::default());
    let s = sequencer(&backing, 0);
    s.reconcile(41).await.unwrap();
    assert_eq!(s.reserve().await.unwrap(), 42);
    s.reconcile(10).await.unwrap(); // behind current — ignored
    assert_eq!(s.reserve().await.unwrap(), 43);
}

#[tokio::test]
async fn invalid_stored_values_are_ignored_self_heal_from_pairing_base() {
    let backing = Arc::new(MemoryBacking::default());
    backing.set_value(DEVICE, -7);
    let s = sequencer(&backing, 3);
    assert_eq!(s.reserve().await.unwrap(), 3);
}

#[tokio::test]
async fn pairing_next_sequence_acts_as_a_floor_over_stale_storage() {
    let backing = Arc::new(MemoryBacking::default());
    backing.set_value(DEVICE, 2);
    let s = sequencer(&backing, 10);
    assert_eq!(s.reserve().await.unwrap(), 10);
}

#[tokio::test]
async fn throws_when_the_sequence_space_is_exhausted() {
    let backing = Arc::new(MemoryBacking::default());
    backing.set_value(DEVICE, MAXIMUM_SAFE_WIRE_INTEGER as i64);
    let s = sequencer(&backing, 0);
    let err = s.reserve().await.unwrap_err();
    assert!(matches!(err, SequencerError::Exhausted));
    assert_eq!(err.to_string(), "sequence space exhausted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serializes_concurrent_reserves_unique_gapless_when_no_crash() {
    let backing = Arc::new(MemoryBacking::default());
    let s = Arc::new(sequencer(&backing, 0));
    let mut join_set = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let s = s.clone();
        join_set.spawn(async move { s.reserve().await.unwrap() });
    }
    let mut reserved = Vec::new();
    while let Some(result) = join_set.join_next().await {
        reserved.push(result.unwrap());
    }
    assert_eq!(reserved.iter().copied().collect::<HashSet<u64>>().len(), 20);
    reserved.sort_unstable();
    assert_eq!(reserved, (0..20).collect::<Vec<u64>>());
}
