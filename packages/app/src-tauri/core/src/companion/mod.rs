//! Companion service layer: HTTP transport, presence writer, pairing,
//! Live Desk coordinator and the service facade.
//!
//! Scaffold-owned mod file. Ownership: agent T owns
//! {http_client, errors, presence_client}; agent K owns
//! {pairing, authority, consent_gate, coordinator, service}.

pub mod authority;
pub mod consent_gate;
pub mod coordinator;
pub mod errors;
pub mod http_client;
pub mod pairing;
pub mod presence_client;
pub mod service;

pub use authority::{AuthorityRegistry, PublishAuthority};
pub use consent_gate::{projection_of, Confirmation, ConsentGate};
pub use coordinator::{CoordinatorDeps, CoordinatorTimings, LiveDeskCoordinator};
pub use errors::{
    accepted_sequence_of, is_safe_for_immediate_idempotent_retry, needs_renegotiation,
    CompanionTransportError,
};
pub use http_client::{
    CompanionHttpClient, CompanionServerConfiguration, CompanionServerConfigurationError,
    ExecuteOptions, HttpMethod,
};
pub use pairing::{claim_pairing, ClaimPairingError, PairingError, PairingErrorCode, PairingResult};
pub use presence_client::{PresenceClient, PresenceClientError};
pub use service::{CompanionService, CompanionServiceDeps, PublishTelemetry, ServiceError};

// Cross-module credential type lives in model.rs (see integration-notes on
// the ARCHITECTURE placement note).
pub use crate::model::CompanionCredential;
