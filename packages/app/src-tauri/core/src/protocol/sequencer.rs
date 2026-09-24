//! Durable monotonic sequence reservation/reconciliation.
//!
//! Owned by agent P. Ground truth:
//! `packages/core/src/companion/sequencer.ts` and
//! `.claude/rewrite/specs/protocol-transport.md` §6. The stored value is
//! "the NEXT sequence to hand out"; there is NO in-memory cache — every
//! reserve/reconcile re-loads from persistence. All operations run through a
//! strict FIFO critical section (a failed task must not poison the queue);
//! `tokio::sync::Mutex` is FIFO-fair and suits this.

use std::sync::Arc;

use async_trait::async_trait;

use crate::model::MAXIMUM_SAFE_WIRE_INTEGER;

/// Persistence interface (TS `SequencePersistence`), implemented by
/// `store::sequence::FileSequenceStore`.
///
/// Contract deviation from the ARCHITECTURE sketch (recorded in
/// integration-notes): both methods take `device_id` (the backing file maps
/// deviceId -> next sequence) and `load` returns `Option<i64>` — raw stored
/// values may be negative/absent/corrupt; VALIDITY (integer in
/// `0..=2^53-1`) is judged by the sequencer, which self-heals invalid values
/// to the pairing floor. Non-integer JSON numbers are mapped to `None` by
/// the store (behavior-equivalent merge of the two TS rejection layers).
#[async_trait]
pub trait SequenceBacking: Send + Sync {
    async fn load(&self, device_id: &str) -> Option<i64>;
    /// Durable write; failure (e.g. disk full) must fail the surrounding
    /// operation without consuming a sequence.
    async fn store(&self, device_id: &str, next: u64) -> std::io::Result<()>;
}

#[derive(Debug, thiserror::Error)]
pub enum SequencerError {
    /// `current >= 2^53-1`: the maximum value ever RETURNED is 2^53-2, the
    /// maximum ever STORED is 2^53-1 (TS error name
    /// `SequenceExhaustedError`).
    #[error("sequence space exhausted")]
    Exhausted,
    /// Persistence failure propagated out of reserve/reconcile; the message
    /// must pass through verbatim (test asserts `"disk full"`).
    #[error(transparent)]
    Backing(#[from] std::io::Error),
}

/// Ordered sequence authority for one paired device.
pub struct CompanionSequencer {
    backing: Arc<dyn SequenceBacking>,
    device_id: String,
    pairing_next_sequence: u64,
    /// FIFO critical section serializing reserve/reconcile (tokio's Mutex is
    /// FIFO-fair; a failed task releases the guard without poisoning the
    /// queue).
    slot: tokio::sync::Mutex<()>,
}

impl CompanionSequencer {
    /// `pairing_next_sequence` is a hard floor over whatever the backing
    /// holds (`currentNext = max(floor, valid stored value)`).
    pub fn new(
        backing: Arc<dyn SequenceBacking>,
        device_id: impl Into<String>,
        pairing_next_sequence: u64,
    ) -> Self {
        Self {
            backing,
            device_id: device_id.into(),
            pairing_next_sequence,
            slot: tokio::sync::Mutex::new(()),
        }
    }

    /// The next sequence to hand out. Re-loads from persistence every time
    /// (no in-memory cache); invalid/corrupt stored values self-heal to the
    /// pairing floor.
    async fn current_next(&self) -> u64 {
        let stored = self.backing.load(&self.device_id).await;
        let valid = stored
            .filter(|&v| v >= 0 && v as u64 <= MAXIMUM_SAFE_WIRE_INTEGER)
            .map(|v| v as u64);
        self.pairing_next_sequence
            .max(valid.unwrap_or(self.pairing_next_sequence))
    }

    /// Reserve the next sequence: `current + 1` is PERSISTED before
    /// `current` is returned (and therefore before any network send). A
    /// crash between persist and send produces a legal gap; a sequence
    /// number is NEVER reused. A store failure propagates and consumes
    /// nothing.
    pub async fn reserve(&self) -> Result<u64, SequencerError> {
        let _slot = self.slot.lock().await;
        let current = self.current_next().await;
        if current >= MAXIMUM_SAFE_WIRE_INTEGER {
            return Err(SequencerError::Exhausted);
        }
        // Persistence is the linearization point.
        self.backing.store(&self.device_id, current + 1).await?;
        Ok(current)
    }

    /// Advance the stored next to `accepted_sequence + 1`, never backwards;
    /// a stale/behind value is ignored without a store call. Silent no-op
    /// when `accepted_sequence >= 2^53-1` (`accepted + 1` would leave the
    /// wire range; negative/non-integer inputs are unrepresentable in u64).
    pub async fn reconcile(&self, accepted_sequence: u64) -> Result<(), SequencerError> {
        let _slot = self.slot.lock().await;
        if accepted_sequence >= MAXIMUM_SAFE_WIRE_INTEGER {
            return Ok(());
        }
        let current = self.current_next().await;
        let next = current.max(accepted_sequence + 1);
        if next != current {
            self.backing.store(&self.device_id, next).await?;
        }
        Ok(())
    }
}
