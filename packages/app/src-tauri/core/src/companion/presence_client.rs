//! Ordered presence writer: FIFO send slot, durable sequence reservation,
//! single idempotent retry with identical bytes, acceptedSequence
//! reconciliation.
//!
//! Owned by agent T. Ground truth:
//! `packages/core/src/companion/presenceClient.ts` and
//! `.claude/rewrite/specs/protocol-transport.md` §9. A mutation's ENTIRE
//! lifecycle (reserve -> map -> device guard -> send + at-most-one retry ->
//! reconcile) is ONE FIFO critical section in call order; failures do not
//! poison the slot. Encode ONCE: the retry resends the byte-identical
//! buffer with the same sequence and requestId.

use std::sync::Arc;

use serde::Serialize;

use crate::companion::errors::{
    accepted_sequence_of, is_safe_for_immediate_idempotent_retry, needs_renegotiation,
    CompanionTransportError,
};
use crate::companion::http_client::{self, CompanionHttpClient, ExecuteOptions, HttpMethod};
use crate::model::{
    ClearReason, CompanionCredential, MutationResponse, NegotiatedPresenceConfiguration,
    SanitizedPresenceSnapshot,
};
use crate::protocol::dto_mapper::{make_clear_request, make_presence_request, MakeRequestOptions};
use crate::protocol::sequencer::{CompanionSequencer, SequencerError};
use crate::protocol::types::decode_mutation_response;
use crate::protocol::wire::WireError;

/// Everything `replace_presence`/`clear_presence` can fail with. Mapper
/// `WireError`s and `reserve()` failures propagate without any HTTP call
/// (a mapped-then-failed request leaves a legal sequence gap).
#[derive(Debug, thiserror::Error)]
pub enum PresenceClientError {
    #[error(transparent)]
    Wire(#[from] WireError),
    #[error(transparent)]
    Sequencer(#[from] SequencerError),
    #[error(transparent)]
    Transport(#[from] CompanionTransportError),
}

impl PresenceClientError {
    /// [`crate::companion::errors::is_safe_for_immediate_idempotent_retry`]
    /// on the transport variant; `Wire`/`Sequencer` are never retry-safe.
    pub fn is_retry_safe(&self) -> bool {
        match self {
            PresenceClientError::Transport(error) => is_safe_for_immediate_idempotent_retry(error),
            _ => false,
        }
    }

    /// [`crate::companion::errors::needs_renegotiation`] on the transport
    /// variant, else false. This is what the coordinator branches on.
    pub fn needs_renegotiation(&self) -> bool {
        match self {
            PresenceClientError::Transport(error) => needs_renegotiation(error),
            _ => false,
        }
    }

    /// [`crate::companion::errors::accepted_sequence_of`] on the transport
    /// variant, else `None`.
    pub fn accepted_sequence(&self) -> Option<u64> {
        match self {
            PresenceClientError::Transport(error) => accepted_sequence_of(error),
            _ => None,
        }
    }
}

/// One PresenceClient per negotiated generation. It does NOT survive
/// renegotiation (payload/lease limits always come from the newest
/// capabilities); the shared [`CompanionSequencer`] does.
pub struct PresenceClient {
    http: Arc<CompanionHttpClient>,
    credential: CompanionCredential,
    sequencer: Arc<CompanionSequencer>,
    configuration: NegotiatedPresenceConfiguration,
    /// FIFO send slot (tokio's Mutex queues waiters fairly, so concurrent
    /// mutations are processed strictly in lock-request order — the Rust
    /// equivalent of the TS promise-chain slot). Held across the whole
    /// mutation lifecycle; RAII release means failures cannot poison it.
    send_slot: tokio::sync::Mutex<()>,
}

impl PresenceClient {
    pub fn new(
        http: Arc<CompanionHttpClient>,
        credential: CompanionCredential,
        sequencer: Arc<CompanionSequencer>,
        configuration: NegotiatedPresenceConfiguration,
    ) -> Self {
        Self {
            http,
            credential,
            sequencer,
            configuration,
            send_slot: tokio::sync::Mutex::new(()),
        }
    }

    /// `PUT /companion/presence` inside the slot: reserve (durable persist
    /// BEFORE send) -> `make_presence_request` -> device guard -> single
    /// retry -> reconcile `data.acceptedSequence` (and, from `Server`
    /// errors, `error.acceptedSequence` BEFORE the retry decision).
    pub async fn replace_presence(
        &self,
        snapshot: &SanitizedPresenceSnapshot,
        requested_lease_seconds: f64,
    ) -> Result<MutationResponse, PresenceClientError> {
        let _slot = self.send_slot.lock().await;
        let sequence = self.sequencer.reserve().await?;
        let mapped = make_presence_request(
            snapshot,
            sequence,
            requested_lease_seconds,
            &self.make_request_options(),
        )?;
        self.assert_device_matches(&mapped.body.meta.device_id)?;
        self.perform_with_single_retry(
            HttpMethod::Put,
            "/companion/presence",
            &mapped.request_id,
            &mapped.body,
        )
        .await
    }

    /// `POST /companion/presence/clear`: consumes a sequence like any
    /// mutation; identical slot/retry/reconcile shape.
    pub async fn clear_presence(
        &self,
        reason: ClearReason,
        observed_at_ms: i64,
    ) -> Result<MutationResponse, PresenceClientError> {
        let _slot = self.send_slot.lock().await;
        let sequence = self.sequencer.reserve().await?;
        let mapped = make_clear_request(
            reason,
            sequence,
            observed_at_ms,
            &self.make_request_options(),
        )?;
        self.assert_device_matches(&mapped.body.meta.device_id)?;
        self.perform_with_single_retry(
            HttpMethod::Post,
            "/companion/presence/clear",
            &mapped.request_id,
            &mapped.body,
        )
        .await
    }

    fn make_request_options(&self) -> MakeRequestOptions {
        MakeRequestOptions {
            device_id: self.credential.device_id.clone(),
            lease_min_seconds: self.configuration.lease_min_seconds,
            lease_max_seconds: self.configuration.lease_max_seconds,
        }
    }

    /// Defense-in-depth: the mapper received the deviceId FROM the
    /// credential, so this can only fire on internal wiring bugs. When it
    /// fires, the reserved sequence is consumed as a legal gap — nothing is
    /// sent and nothing retried (spec ambiguity 5).
    // The error enum's size is a scaffold contract shape (inline
    // ErrorEnvelope); cold path, perf lint knowingly waived.
    #[allow(clippy::result_large_err)]
    fn assert_device_matches(&self, meta_device_id: &str) -> Result<(), PresenceClientError> {
        if meta_device_id != self.credential.device_id {
            return Err(PresenceClientError::Transport(
                CompanionTransportError::CredentialDeviceMismatch,
            ));
        }
        Ok(())
    }

    async fn perform_with_single_retry<B: Serialize>(
        &self,
        method: HttpMethod,
        path: &str,
        request_id: &str,
        body: &B,
    ) -> Result<MutationResponse, PresenceClientError> {
        // Encode once: a retry must resend the exact same bytes (same
        // sequence, same requestId — idempotent resend, never a re-map).
        let encoded_body = http_client::encode_body(body);
        let attempt = || {
            self.http.execute(
                ExecuteOptions {
                    method,
                    path,
                    credential: Some(&self.credential),
                    expected_request_id: Some(request_id),
                    // The NEGOTIATED cap, not the 32768 default.
                    maximum_payload_bytes: Some(self.configuration.maximum_payload_bytes),
                    encoded_body: Some(&encoded_body),
                },
                decode_mutation_response,
            )
        };

        match attempt().await {
            Ok(response) => {
                self.sequencer
                    .reconcile(response.data.accepted_sequence)
                    .await?;
                Ok(response)
            }
            Err(error) => {
                // Reconcile from the error BEFORE the retry decision (the
                // retry still resends the ORIGINAL sequence bytes even if
                // this advanced the store).
                self.reconcile_from_error(&error).await?;
                if !is_safe_for_immediate_idempotent_retry(&error) {
                    return Err(error.into());
                }
                // The only retry — total attempts: 2, NEVER more.
                match attempt().await {
                    Ok(response) => {
                        self.sequencer
                            .reconcile(response.data.accepted_sequence)
                            .await?;
                        Ok(response)
                    }
                    Err(retry_error) => {
                        self.reconcile_from_error(&retry_error).await?;
                        Err(retry_error.into())
                    }
                }
            }
        }
    }

    async fn reconcile_from_error(
        &self,
        error: &CompanionTransportError,
    ) -> Result<(), SequencerError> {
        if let Some(accepted) = accepted_sequence_of(error) {
            self.sequencer.reconcile(accepted).await?;
        }
        Ok(())
    }
}
