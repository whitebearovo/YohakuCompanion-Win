//! Pairing client: the one-time pairing code is consumed only AFTER (1) the
//! credential store proved writable and (2) capabilities negotiated
//! successfully — never burn a code for a token that could not be stored or
//! used.
//!
//! Owned by agent K. Ground truth: `packages/core/src/companion/pairing.ts`
//! and `.claude/rewrite/specs/companion-service.md` §5. Step order: validate
//! code (trim, 1..=32 UTF-16 units) -> validate name (NFC + trim, 1..=120
//! Unicode scalars) -> validate base URL -> ensure credential store ->
//! unauthenticated GET capabilities (ANY failure -> `Network`) -> negotiate
//! -> POST claim -> require scope `companion:presence:write`. The returned
//! `base_url` is the URL object's NORMALIZED serialization (this exact
//! string is persisted and later feeds the authority key).

use std::future::Future;

use unicode_normalization::UnicodeNormalization;

use crate::companion::errors::CompanionTransportError;
use crate::companion::http_client::{
    encode_body, CompanionHttpClient, CompanionServerConfiguration, ExecuteOptions, HttpMethod,
};
use crate::protocol::capabilities::{negotiate_presence, PresenceNegotiation};
use crate::protocol::types::{
    decode_capabilities_response, decode_pairing_claim_response, PairingClaimRequest,
    REQUIRED_PRESENCE_SCOPE,
};
use crate::protocol::wire::PROTOCOL_CLIENT_VERSION;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingErrorCode {
    InvalidPairingCode,
    InvalidDeviceName,
    InvalidServerUrl,
    ClientUpdateRequired,
    ServerFeatureUnavailable,
    InvalidCapabilities,
    RequiredScopeMissing,
    PairingRejected,
    Network,
}

/// Pairing failure; `server_code` is the server error code when available
/// (e.g. `COMPANION_PAIRING_EXPIRED` for a rejected claim).
#[derive(Debug, thiserror::Error)]
#[error("pairing failed: {code:?}")]
pub struct PairingError {
    pub code: PairingErrorCode,
    pub server_code: Option<String>,
}

impl PairingError {
    fn new(code: PairingErrorCode) -> Self {
        Self {
            code,
            server_code: None,
        }
    }
}

/// `claim_pairing` failure. `Other` covers non-pairing throws (the
/// `ensure_credential_store` hook) which the service maps to
/// `IpcErrorCode::PairingFailed`, mirroring the TS "any non-PairingError"
/// row of the mapping table.
#[derive(Debug, thiserror::Error)]
pub enum ClaimPairingError {
    #[error(transparent)]
    Pairing(#[from] PairingError),
    #[error("{0}")]
    Other(String),
}

/// Claim result: `PairingClaimData` fields plus the normalized base URL and
/// the normalized device name (both persisted verbatim).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingResult {
    pub device_id: String,
    pub device_token: String,
    pub scopes: Vec<String>,
    pub next_sequence: u64,
    pub base_url: String,
    pub device_name: String,
}

/// Free function (TS `claimPairing`). `ensure_credential_store` is awaited
/// after input validation and before any network call; its failure becomes
/// `ClaimPairingError::Other`.
pub async fn claim_pairing<Fut>(
    base_url: &str,
    device_name: &str,
    pairing_code: &str,
    ensure_credential_store: Fut,
) -> Result<PairingResult, ClaimPairingError>
where
    Fut: Future<Output = Result<(), String>> + Send,
{
    let code = pairing_code.trim();
    // TS `code.length` is UTF-16 code-unit length.
    let code_units = code.encode_utf16().count();
    if !(1..=32).contains(&code_units) {
        return Err(PairingError::new(PairingErrorCode::InvalidPairingCode).into());
    }

    let name: String = device_name.nfc().collect::<String>().trim().to_string();
    // TS `[...name].length` counts Unicode scalar values.
    if name.is_empty() || name.chars().count() > 120 {
        return Err(PairingError::new(PairingErrorCode::InvalidDeviceName).into());
    }

    let configuration = match CompanionServerConfiguration::new(base_url.trim()) {
        Ok(configuration) => configuration,
        Err(_) => return Err(PairingError::new(PairingErrorCode::InvalidServerUrl).into()),
    };
    // URL.toString() serialization — this exact normalized string is
    // persisted and later feeds the authority key.
    let normalized_base_url = configuration.base_url().to_string();
    let http = CompanionHttpClient::new(configuration);

    // Preflight 1: protected storage must be writable before consuming the
    // code. A failure here is a non-PairingError (-> Other -> pairingFailed).
    ensure_credential_store
        .await
        .map_err(ClaimPairingError::Other)?;

    // Preflight 2: the protocol must be usable before consuming the code.
    // ANY failure (network, decode, non-2xx, even a decodable server error
    // envelope) maps to `network`.
    let capabilities = match http
        .execute(
            ExecuteOptions {
                method: HttpMethod::Get,
                path: "/companion/capabilities",
                credential: None,
                expected_request_id: None,
                maximum_payload_bytes: None,
                encoded_body: None,
            },
            decode_capabilities_response,
        )
        .await
    {
        Ok(capabilities) => capabilities,
        Err(_) => return Err(PairingError::new(PairingErrorCode::Network).into()),
    };

    match negotiate_presence(&capabilities.data, PROTOCOL_CLIENT_VERSION) {
        PresenceNegotiation::ClientUpdateRequired => {
            return Err(PairingError::new(PairingErrorCode::ClientUpdateRequired).into());
        }
        PresenceNegotiation::SchemaUnsupported | PresenceNegotiation::FeatureUnavailable => {
            return Err(PairingError::new(PairingErrorCode::ServerFeatureUnavailable).into());
        }
        PresenceNegotiation::InvalidCapabilities => {
            return Err(PairingError::new(PairingErrorCode::InvalidCapabilities).into());
        }
        PresenceNegotiation::Available { .. } => {}
    }

    let body = PairingClaimRequest {
        device_name: name.clone(),
        pairing_code: code.to_string(),
    };
    let encoded = encode_body(&body);
    let claim = match http
        .execute(
            ExecuteOptions {
                method: HttpMethod::Post,
                path: "/companion/pairings/claim",
                credential: None,
                expected_request_id: None,
                maximum_payload_bytes: None,
                encoded_body: Some(&encoded),
            },
            decode_pairing_claim_response,
        )
        .await
    {
        Ok(claim) => claim,
        Err(error) => {
            return Err(match extract_pairing_server_code(&error) {
                Some(server_code) => PairingError {
                    code: PairingErrorCode::PairingRejected,
                    server_code: Some(server_code),
                }
                .into(),
                None => PairingError::new(PairingErrorCode::Network).into(),
            });
        }
    };

    if !claim
        .data
        .scopes
        .iter()
        .any(|scope| scope == REQUIRED_PRESENCE_SCOPE)
    {
        return Err(PairingError::new(PairingErrorCode::RequiredScopeMissing).into());
    }

    Ok(PairingResult {
        device_id: claim.data.device_id,
        device_token: claim.data.device_token,
        scopes: claim.data.scopes,
        next_sequence: claim.data.next_sequence,
        base_url: normalized_base_url,
        device_name: name,
    })
}

/// `CompanionPairingServerError` (simplified `{error:{code}}` envelope) ->
/// its code; `CompanionServerError` (full envelope) -> `envelope.error.code`;
/// anything else -> `None`.
fn extract_pairing_server_code(error: &CompanionTransportError) -> Option<String> {
    match error {
        CompanionTransportError::PairingServer { code, .. } => Some(code.clone()),
        CompanionTransportError::Server { envelope, .. } => Some(envelope.error.code.clone()),
        _ => None,
    }
}
