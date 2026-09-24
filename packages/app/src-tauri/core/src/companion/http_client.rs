//! HTTP layer for Companion Protocol v2 (reqwest, schannel TLS).
//!
//! Owned by agent T. Ground truth:
//! `packages/core/src/companion/transport/httpClient.ts` and
//! `.claude/rewrite/specs/protocol-transport.md` §8. Base URL must be HTTPS
//! (loopback HTTP allowed), no credentials/query/fragment. Bearer token +
//! version header are attached only alongside a credential; the
//! capabilities GET is fully unauthenticated. 10 s whole-request timeout;
//! encoded payloads are size-checked BEFORE any network I/O; response
//! requestId must echo the request's. There is NO response size cap and NO
//! response Content-Type check (matching TS).

use serde::Serialize;

use crate::companion::errors::CompanionTransportError;
use crate::protocol::types::{decode_error_envelope, decode_pairing_error_envelope};
use crate::protocol::wire::PROTOCOL_CLIENT_VERSION;

pub use crate::model::CompanionCredential;

/// Whole-request timeout (maps to `CompanionTransportError::Network`).
pub const REQUEST_TIMEOUT_MS: u64 = 10_000;
/// Payload cap when no negotiated cap is supplied (pairing claim).
pub const DEFAULT_MAX_PAYLOAD_BYTES: usize = 32 * 1024;
/// `X-Yohaku-Companion-Version` (exact spelling on the wire).
pub const VERSION_HEADER: &str = "X-Yohaku-Companion-Version";

/// Loopback per httpClient.ts: `localhost`, `::1`, `[::1]`, or a 4-part
/// dotted IPv4 with first octet `127` and every octet `^\d{1,3}$` and
/// <= 255 (leading zeros allowed; `"127.1"` shorthand and other IPv6
/// spellings are NOT loopback).
pub fn is_loopback_host(host: &str) -> bool {
    let lowered = host.to_lowercase();
    if lowered == "localhost" || lowered == "::1" || lowered == "[::1]" {
        return true;
    }
    let parts: Vec<&str> = lowered.split('.').collect();
    if parts.len() != 4 || parts[0] != "127" {
        return false;
    }
    parts.iter().all(|part| {
        // JS: /^\d{1,3}$/.test(p) && parseInt(p, 10) <= 255
        (1..=3).contains(&part.len())
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && part.parse::<u32>().is_ok_and(|octet| octet <= 255)
    })
}

/// Base URL validation failures; messages are exact TS literals.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompanionServerConfigurationError {
    #[error("invalid base URL")]
    InvalidUrl,
    #[error("base URL must use HTTPS (HTTP is allowed only for loopback hosts)")]
    SchemeNotAllowed,
    #[error("base URL must not embed credentials")]
    EmbeddedCredentials,
    #[error("base URL must not contain query or fragment")]
    QueryOrFragment,
}

/// Validated base URL. The parsed URL is kept (WHATWG normalization:
/// lowercased scheme/host, default ports stripped); its `to_string()` is
/// what pairing persists and what feeds the authority key. The `url` crate
/// implements the same WHATWG URL Standard as the JS `URL` class, so
/// serialization matches `URL.toString()` for user-enterable inputs
/// (trailing slash on origin-only URLs included).
pub struct CompanionServerConfiguration {
    base_url: url::Url,
}

impl CompanionServerConfiguration {
    /// Validation, in TS order: parse -> scheme (https, or http on a
    /// loopback host) -> no embedded credentials -> no query/fragment.
    /// Any path prefix is preserved.
    pub fn new(base_url: &str) -> Result<Self, CompanionServerConfigurationError> {
        let parsed =
            url::Url::parse(base_url).map_err(|_| CompanionServerConfigurationError::InvalidUrl)?;
        // url::Url::scheme() is already lowercase (WHATWG parse), like the
        // TS `protocol.replace(":", "").toLowerCase()`.
        let scheme = parsed.scheme();
        // JS URL.hostname keeps brackets on IPv6 hosts; url::Url::host_str
        // does too, which is what makes the "[::1]" arm reachable.
        let hostname = parsed.host_str().unwrap_or("");
        if scheme != "https" && !(scheme == "http" && is_loopback_host(hostname)) {
            return Err(CompanionServerConfigurationError::SchemeNotAllowed);
        }
        // JS compares username/password against "" — an empty password
        // (e.g. "https://:@host") passes, any non-empty component fails.
        if !parsed.username().is_empty()
            || parsed
                .password()
                .is_some_and(|password| !password.is_empty())
        {
            return Err(CompanionServerConfigurationError::EmbeddedCredentials);
        }
        // JS URL.search / URL.hash are "" for both an absent and an EMPTY
        // query/fragment ("https://x/?" passes) — replicate that exactly.
        if parsed.query().is_some_and(|query| !query.is_empty())
            || parsed
                .fragment()
                .is_some_and(|fragment| !fragment.is_empty())
        {
            return Err(CompanionServerConfigurationError::QueryOrFragment);
        }
        Ok(Self { base_url: parsed })
    }

    /// The validated, normalized base URL.
    pub fn base_url(&self) -> &url::Url {
        &self.base_url
    }

    /// Join `path` onto the base: strip AT MOST ONE trailing `/` from the
    /// base path (a base path ending in `//` therefore keeps a double
    /// slash — spec ambiguity 11), ensure the suffix starts with `/`,
    /// concatenate.
    pub fn endpoint(&self, path: &str) -> url::Url {
        let mut url = self.base_url.clone();
        let base_path = self.base_url.path();
        let base_path = base_path.strip_suffix('/').unwrap_or(base_path);
        let joined = if path.starts_with('/') {
            format!("{base_path}{path}")
        } else {
            format!("{base_path}/{path}")
        };
        url.set_path(&joined);
        url
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
}

impl HttpMethod {
    fn as_reqwest(self) -> reqwest::Method {
        match self {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
            HttpMethod::Put => reqwest::Method::PUT,
        }
    }
}

/// Options for one attempt. Scaffold decision (recorded in
/// integration-notes): the TS `body`/`encodedBody` pair collapses into
/// `encoded_body` — callers ALWAYS pre-encode via [`encode_body`], which is
/// what TS did internally anyway, and retries pass the identical bytes.
pub struct ExecuteOptions<'a> {
    pub method: HttpMethod,
    pub path: &'a str,
    /// Attach `Authorization: Bearer <deviceToken>` + the version header.
    pub credential: Option<&'a CompanionCredential>,
    /// When set, `meta.requestId` of ANY decoded envelope (success or error)
    /// must equal it, else `RequestIdMismatch` (checked BEFORE `Server`).
    pub expected_request_id: Option<&'a str>,
    /// Cap for the encoded body; `None` = [`DEFAULT_MAX_PAYLOAD_BYTES`].
    /// Strictly-greater fails (equal passes), before any network I/O.
    pub maximum_payload_bytes: Option<usize>,
    /// Pre-encoded request bytes (also implies `Content-Type:
    /// application/json`); `None` for bodyless requests (GET).
    pub encoded_body: Option<&'a [u8]>,
}

/// Compact UTF-8 JSON bytes of `body` — byte-identical to
/// `JSON.stringify` for our request types (insertion-order keys, explicit
/// nulls, ECMAScript number formatting via the types' serializers).
pub fn encode_body<B: Serialize>(body: &B) -> Vec<u8> {
    serde_json::to_vec(body).expect("request bodies serialize infallibly (string keys, no NaN)")
}

/// Companion HTTP client over reqwest (native-tls => schannel on Windows).
pub struct CompanionHttpClient {
    configuration: CompanionServerConfiguration,
    client: reqwest::Client,
}

impl CompanionHttpClient {
    pub fn new(configuration: CompanionServerConfiguration) -> Self {
        // Whole-request timeout, the reqwest equivalent of the TS
        // `AbortSignal.timeout(10_000)` (covers send + body read; a timeout
        // is indistinguishable from any other network failure).
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(REQUEST_TIMEOUT_MS))
            .build()
            .expect("reqwest client construction (TLS backend init)");
        Self {
            configuration,
            client,
        }
    }

    /// One HTTP attempt with the full response-handling matrix of spec §8.6
    /// (network / empty body / non-JSON / schema / requestId echo / error
    /// envelope / pairing envelope / bare status). `response_schema` is the
    /// zod-schema equivalent from `protocol::types` (its `Err` string
    /// becomes `Decode("response decode failed: <first issue>")`).
    pub async fn execute<T>(
        &self,
        options: ExecuteOptions<'_>,
        response_schema: fn(&serde_json::Value) -> Result<T, String>,
    ) -> Result<T, CompanionTransportError> {
        let url = self.configuration.endpoint(options.path);
        let mut request = self
            .client
            .request(options.method.as_reqwest(), url)
            .header("Accept", "application/json");
        if let Some(credential) = options.credential {
            request = request
                .header(
                    "Authorization",
                    format!("Bearer {}", credential.device_token),
                )
                .header(VERSION_HEADER, PROTOCOL_CLIENT_VERSION);
        }
        if let Some(bytes) = options.encoded_body {
            // Payload cap: strictly greater fails, BEFORE any network I/O,
            // deterministically on every attempt (retry bytes are identical).
            let limit = options
                .maximum_payload_bytes
                .unwrap_or(DEFAULT_MAX_PAYLOAD_BYTES);
            if bytes.len() > limit {
                return Err(CompanionTransportError::PayloadTooLarge {
                    actual: bytes.len(),
                    limit,
                });
            }
            request = request
                .header("Content-Type", "application/json")
                .body(bytes.to_vec());
        }

        let response = request.send().await.map_err(network_error)?;
        let status = response.status().as_u16();
        // JS `response.ok`: 200..=299.
        let ok = response.status().is_success();
        let bytes = response.bytes().await.map_err(network_error)?;
        // TS reads response.text() (lossy UTF-8 decode) with no size cap.
        let text = String::from_utf8_lossy(&bytes);
        if text.is_empty() {
            return Err(CompanionTransportError::EmptyResponse { status });
        }

        let json: serde_json::Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(_) if ok => {
                return Err(CompanionTransportError::Decode {
                    message: "response is not JSON".to_string(),
                })
            }
            Err(_) => return Err(CompanionTransportError::HttpStatus { status }),
        };

        if ok {
            let parsed =
                response_schema(&json).map_err(|first_issue| CompanionTransportError::Decode {
                    message: format!("response decode failed: {first_issue}"),
                })?;
            check_request_id_echo(&json, options.expected_request_id)?;
            return Ok(parsed);
        }

        if let Ok(envelope) = decode_error_envelope(&json) {
            // Echo check FIRST: a non-2xx envelope with a wrong requestId
            // surfaces as retry-safe RequestIdMismatch, NOT Server — so its
            // acceptedSequence is never reconciled (spec ambiguity 15).
            check_request_id_echo(&json, options.expected_request_id)?;
            return Err(CompanionTransportError::Server { status, envelope });
        }
        if let Ok(pairing) = decode_pairing_error_envelope(&json) {
            return Err(CompanionTransportError::PairingServer {
                status,
                code: pairing.error.code,
            });
        }
        Err(CompanionTransportError::HttpStatus { status })
    }
}

/// TS: `network failure: ${String(cause)}` — informational; the source
/// chain is appended because reqwest's top-level Display often hides the
/// underlying I/O cause.
fn network_error(error: reqwest::Error) -> CompanionTransportError {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    CompanionTransportError::Network { message }
}

/// Performed only when `expected` is provided; loose lookup of
/// `payload.meta.requestId` (missing or non-string = mismatch), strict
/// string comparison — exactly `checkRequestIdEcho`.
// The error enum's size is a scaffold contract shape (inline ErrorEnvelope);
// these Results are on cold paths, so the perf lint is knowingly waived.
#[allow(clippy::result_large_err)]
fn check_request_id_echo(
    payload: &serde_json::Value,
    expected: Option<&str>,
) -> Result<(), CompanionTransportError> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let echoed = payload
        .get("meta")
        .and_then(|meta| meta.get("requestId"))
        .and_then(|id| id.as_str());
    if echoed != Some(expected) {
        return Err(CompanionTransportError::RequestIdMismatch);
    }
    Ok(())
}
