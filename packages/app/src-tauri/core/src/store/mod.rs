//! Persistence: config.json, sequence.json, credential backends.
//!
//! Scaffold-owned mod file. Module bodies are owned by agent S (who also
//! owns `runtime::logger`). On-disk formats are compatibility invariants —
//! existing installations must keep working byte-for-byte (paths, JSON
//! layouts, Credential Manager target names, DPAPI blob format,
//! `YOHAKU_DATA_DIR` override).

pub mod config;
pub mod credentials;
pub mod sequence;

#[cfg(windows)]
pub mod dpapi_store;
#[cfg(windows)]
pub mod keyring_store;

pub use config::{atomic_write_json, data_directory, ConfigStore, ConfigStoreError};
pub use credentials::{select_credential_store, CredentialStore, CredentialStoreError};
pub use sequence::FileSequenceStore;

#[cfg(windows)]
pub use dpapi_store::DpapiCredentialStore;
#[cfg(windows)]
pub use keyring_store::KeyringCredentialStore;
