//! Transport error taxonomy, retry-safety and renegotiation classification.
//!
//! Owned by agent T. Ground truth:
//! `packages/core/src/companion/transport/errors.ts` and
//! `.claude/rewrite/specs/protocol-transport.md` §7. Only the VARIANT (plus
//! `status`/`code`/`envelope` fields) drives logic; message strings are
//! informational except where ported tests assert them.

pub use crate::protocol::types::ErrorEnvelope;

/// One enum replaces the TS `CompanionTransportError` class family
/// (`CompanionNetworkError` -> `Network`, etc.).
#[derive(Debug, thiserror::Error)]
pub enum CompanionTransportError {
    /// Send rejected (DNS/TCP/TLS/timeout) or reading the body failed.
    /// AMBIGUOUS: the server may or may not have committed the mutation.
    #[error("network failure: {message}")]
    Network { message: String },
    /// Zero-byte response body, ANY status (a 204 would hit this too).
    #[error("empty response ({status})")]
    EmptyResponse { status: u16 },
    /// 2xx body not JSON (`"response is not JSON"`) or 2xx JSON failing the
    /// response schema (`"response decode failed: <first issue>"`).
    #[error("{message}")]
    Decode { message: String },
    /// `meta.requestId` of a decoded success OR error envelope did not echo
    /// `expected_request_id`.
    #[error("response requestId does not echo the request")]
    RequestIdMismatch,
    /// Encoded request body exceeds the payload cap (pre-network check).
    #[error("payload {actual} bytes exceeds limit {limit}")]
    PayloadTooLarge { actual: usize, limit: usize },
    /// PresenceClient defense-in-depth guard (mapped deviceId != credential
    /// deviceId); the reserved sequence is consumed as a legal gap.
    #[error("request deviceId does not match credential")]
    CredentialDeviceMismatch,
    /// Non-2xx with a decodable protocol error envelope.
    #[error("server error {status}: {}", envelope.error.code)]
    Server {
        status: u16,
        envelope: ErrorEnvelope,
    },
    /// Non-2xx whose body is not a decodable envelope (incl. non-JSON).
    #[error("http status {status} without decodable envelope")]
    HttpStatus { status: u16 },
    /// Non-2xx matching the simplified pairing envelope `{error:{code}}`.
    #[error("pairing rejected {status}: {code}")]
    PairingServer { status: u16, code: String },
}

/// Exact predicate of spec §7.2, evaluated top to bottom:
/// PayloadTooLarge/CredentialDeviceMismatch -> false; Server -> 5xx AND
/// `envelope.error.retryable`; HttpStatus -> 5xx; EmptyResponse / Decode /
/// RequestIdMismatch / Network -> true. 4xx is NEVER retried.
pub fn is_safe_for_immediate_idempotent_retry(error: &CompanionTransportError) -> bool {
    match error {
        CompanionTransportError::PayloadTooLarge { .. } => false,
        CompanionTransportError::CredentialDeviceMismatch => false,
        // 5xx with an envelope is retried only when the server says so.
        CompanionTransportError::Server { status, envelope } => {
            (500..=599).contains(status) && envelope.error.retryable
        }
        // Bare 5xx (no decodable envelope) is always retried.
        CompanionTransportError::HttpStatus { status } => (500..=599).contains(status),
        CompanionTransportError::EmptyResponse { .. }
        | CompanionTransportError::Decode { .. }
        | CompanionTransportError::RequestIdMismatch => true,
        // Ambiguous transport failure: retry the exact request once.
        CompanionTransportError::Network { .. } => true,
        // TS fallthrough `return false` (PairingServerError matched no arm).
        CompanionTransportError::PairingServer { .. } => false,
    }
}

/// Server code in {COMPANION_SCHEMA_UNSUPPORTED, COMPANION_FEATURE_UNAVAILABLE}
/// or a bare HTTP 426 -> the caller must discard the authority and re-enter
/// capability negotiation (not a transport degradation). A 426 without a
/// decodable envelope is the compatibility signal from servers that cannot
/// encode an envelope this client can read.
pub fn needs_renegotiation(error: &CompanionTransportError) -> bool {
    match error {
        CompanionTransportError::Server { envelope, .. } => {
            crate::protocol::types::MUTATION_RENEGOTIATE_CODES
                .contains(&envelope.error.code.as_str())
        }
        CompanionTransportError::HttpStatus { status } => *status == 426,
        _ => false,
    }
}

/// `Server` -> `envelope.error.accepted_sequence` (itself nullable), else
/// `None`.
pub fn accepted_sequence_of(error: &CompanionTransportError) -> Option<u64> {
    match error {
        CompanionTransportError::Server { envelope, .. } => envelope.error.accepted_sequence,
        _ => None,
    }
}
