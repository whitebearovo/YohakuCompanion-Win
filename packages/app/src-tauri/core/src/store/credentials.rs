//! Credential storage contract + backend selection.
//!
//! Owned by agent S. Ground truth:
//! `packages/core/src/store/credentials.ts` and `capture-stores.md` §9.
//! The stored secret is the RAW companion device token string, keyed by
//! deviceId — not JSON, no wrapping. Tokens never touch config.json, logs,
//! IPC payloads, or exports.

use async_trait::async_trait;

use crate::model::CredentialBackend;
use crate::runtime::logger;

#[derive(Debug, thiserror::Error)]
pub enum CredentialStoreError {
    /// Every candidate backend failed its probe (TS
    /// `CredentialStoreUnavailableError`, exact message).
    #[error("no credential backend available")]
    Unavailable,
    /// Backend operation failure (`set`/`delete`). Content-free message —
    /// never the token.
    #[error("credential backend failure: {0}")]
    Backend(String),
}

/// Backend contract. `get` is infallible-by-contract (any backend failure
/// reads as "no token", matching the TS catch -> null); `delete` is
/// idempotent.
#[async_trait]
pub trait CredentialStore: Send + Sync {
    fn backend(&self) -> CredentialBackend;
    async fn get(&self, device_id: &str) -> Option<String>;
    async fn set(&self, device_id: &str, token: &str) -> Result<(), CredentialStoreError>;
    async fn delete(&self, device_id: &str) -> Result<(), CredentialStoreError>;
}

/// Probe sentinel key (`set` -> `get` -> `delete` round trip; the probe
/// transiently creates a real credential named per the backend's
/// convention).
pub const PROBE_DEVICE_ID: &str = "__yohaku_probe__";

/// Candidate order: `Some(Dpapi)` -> [Dpapi, Keyring]; `None`/`Some(Keyring)`
/// -> [Keyring, Dpapi] (`preferred` pins the backend that stored an
/// existing token so a later probe cannot orphan it). First candidate whose
/// round trip succeeds wins (`info credentials: "using <backend> backend"`;
/// failures log `warn credentials: "<backend> backend unavailable"`); all
/// fail -> `Err(Unavailable)` (surfaces to pairing as
/// `credentialStoreUnavailable`).
pub async fn select_credential_store(
    preferred: Option<CredentialBackend>,
) -> Result<Box<dyn CredentialStore>, CredentialStoreError> {
    for backend in candidate_order(preferred) {
        // Construction failure (data dir unresolvable, mkdir denied) counts
        // as a failed probe for that candidate rather than aborting the whole
        // selection — documented deviation from the TS edge case where an
        // eager constructor throw escaped selectCredentialStore entirely
        // (integration-notes "S — credential backend construction").
        let store = match instantiate(backend) {
            Some(store) => store,
            None => {
                logger::warn(
                    "credentials",
                    &format!("{} backend unavailable", backend_name(backend)),
                );
                continue;
            }
        };
        if round_trips(store.as_ref()).await {
            logger::info(
                "credentials",
                &format!("using {} backend", backend_name(backend)),
            );
            return Ok(store);
        }
        logger::warn(
            "credentials",
            &format!("{} backend unavailable", backend_name(backend)),
        );
    }
    Err(CredentialStoreError::Unavailable)
}

/// `preferred` pins the backend probed first; otherwise keyring leads.
fn candidate_order(preferred: Option<CredentialBackend>) -> [CredentialBackend; 2] {
    match preferred {
        Some(CredentialBackend::Dpapi) => [CredentialBackend::Dpapi, CredentialBackend::Keyring],
        _ => [CredentialBackend::Keyring, CredentialBackend::Dpapi],
    }
}

/// The lowercase backend word used in the pinned log messages (identical to
/// the serde serialization of [`CredentialBackend`]).
fn backend_name(backend: CredentialBackend) -> &'static str {
    match backend {
        CredentialBackend::Keyring => "keyring",
        CredentialBackend::Dpapi => "dpapi",
    }
}

#[cfg(windows)]
fn instantiate(backend: CredentialBackend) -> Option<Box<dyn CredentialStore>> {
    match backend {
        CredentialBackend::Keyring => Some(Box::new(
            crate::store::keyring_store::KeyringCredentialStore::new(),
        )),
        CredentialBackend::Dpapi => crate::store::dpapi_store::DpapiCredentialStore::new(None)
            .ok()
            .map(|store| Box::new(store) as Box<dyn CredentialStore>),
    }
}

/// Non-Windows builds are out of scope (ARCHITECTURE threading note); both
/// backends are Windows-only, so selection can only report unavailability.
#[cfg(not(windows))]
fn instantiate(_backend: CredentialBackend) -> Option<Box<dyn CredentialStore>> {
    None
}

/// TS `roundTrips`: `set` -> `get` -> `delete` on the sentinel; qualifies
/// iff no step failed AND the read value is exactly `"probe"`. Step order
/// matters: a failed `set` skips the rest; `delete` runs (and may fail the
/// probe) even when the read value is already wrong.
async fn round_trips(store: &dyn CredentialStore) -> bool {
    if store.set(PROBE_DEVICE_ID, "probe").await.is_err() {
        return false;
    }
    let read = store.get(PROBE_DEVICE_ID).await;
    if store.delete(PROBE_DEVICE_ID).await.is_err() {
        return false;
    }
    read.as_deref() == Some("probe")
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    /// In-memory fake used to pin the probe algorithm WITHOUT touching the
    /// real Credential Manager / DPAPI file (select_credential_store itself
    /// is deliberately untested here — its probe creates real credentials).
    struct FakeStore {
        fail_set: bool,
        fail_delete: bool,
        get_value: Option<&'static str>,
        calls: Mutex<Vec<&'static str>>,
    }

    impl FakeStore {
        fn new() -> Self {
            Self {
                fail_set: false,
                fail_delete: false,
                get_value: Some("probe"),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl CredentialStore for FakeStore {
        fn backend(&self) -> CredentialBackend {
            CredentialBackend::Keyring
        }

        async fn get(&self, device_id: &str) -> Option<String> {
            assert_eq!(device_id, PROBE_DEVICE_ID);
            self.calls.lock().push("get");
            self.get_value.map(str::to_string)
        }

        async fn set(&self, device_id: &str, token: &str) -> Result<(), CredentialStoreError> {
            assert_eq!(device_id, PROBE_DEVICE_ID);
            assert_eq!(token, "probe");
            self.calls.lock().push("set");
            if self.fail_set {
                return Err(CredentialStoreError::Backend("set failed".to_string()));
            }
            Ok(())
        }

        async fn delete(&self, device_id: &str) -> Result<(), CredentialStoreError> {
            assert_eq!(device_id, PROBE_DEVICE_ID);
            self.calls.lock().push("delete");
            if self.fail_delete {
                return Err(CredentialStoreError::Backend("delete failed".to_string()));
            }
            Ok(())
        }
    }

    #[test]
    fn candidate_order_pins_preferred_backend() {
        assert_eq!(
            candidate_order(Some(CredentialBackend::Dpapi)),
            [CredentialBackend::Dpapi, CredentialBackend::Keyring]
        );
        assert_eq!(
            candidate_order(Some(CredentialBackend::Keyring)),
            [CredentialBackend::Keyring, CredentialBackend::Dpapi]
        );
        assert_eq!(
            candidate_order(None),
            [CredentialBackend::Keyring, CredentialBackend::Dpapi]
        );
    }

    #[test]
    fn backend_names_match_ts_literals() {
        assert_eq!(backend_name(CredentialBackend::Keyring), "keyring");
        assert_eq!(backend_name(CredentialBackend::Dpapi), "dpapi");
    }

    #[test]
    fn unavailable_error_message_is_exact() {
        assert_eq!(
            CredentialStoreError::Unavailable.to_string(),
            "no credential backend available"
        );
    }

    #[tokio::test]
    async fn round_trip_succeeds_in_set_get_delete_order() {
        let store = FakeStore::new();
        assert!(round_trips(&store).await);
        assert_eq!(store.calls.lock().as_slice(), &["set", "get", "delete"]);
    }

    #[tokio::test]
    async fn round_trip_fails_when_set_fails_and_skips_rest() {
        let store = FakeStore {
            fail_set: true,
            ..FakeStore::new()
        };
        assert!(!round_trips(&store).await);
        assert_eq!(
            store.calls.lock().as_slice(),
            &["set"],
            "get/delete skipped after a failed set"
        );
    }

    #[tokio::test]
    async fn round_trip_fails_on_wrong_or_missing_read() {
        for get_value in [None, Some("not-probe")] {
            let store = FakeStore {
                get_value,
                ..FakeStore::new()
            };
            assert!(!round_trips(&store).await);
            // delete still ran (TS: throw-free path reaches the comparison).
            assert_eq!(store.calls.lock().as_slice(), &["set", "get", "delete"]);
        }
    }

    #[tokio::test]
    async fn round_trip_fails_when_delete_fails() {
        let store = FakeStore {
            fail_delete: true,
            ..FakeStore::new()
        };
        assert!(
            !round_trips(&store).await,
            "a delete failure disqualifies even a correct read"
        );
    }
}
