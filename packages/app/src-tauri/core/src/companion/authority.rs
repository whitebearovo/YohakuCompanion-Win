//! One ordered writer per paired `(baseUrl, deviceId)`.
//!
//! Owned by agent K. Ground truth:
//! `packages/core/src/companion/authority.ts` and
//! `.claude/rewrite/specs/companion-service.md` §4. The authority key is the
//! exact string `"{baseUrl}|{deviceId}"` of the PERSISTED (normalized) base
//! URL. The SEQUENCER survives renegotiation, sleep/wake and degraded
//! retries for the same key; the `PresenceClient` is ALWAYS overwritten
//! after `resolve` with one built from the freshest negotiation. `discard`
//! (fresh sequencer object on next resolve, still seeded by the same
//! persistence + pairing floor) happens on: capability rejection during
//! publish, unpair, pairing replacement — NOT on sleep/wake or plain
//! degraded retries.

use std::sync::Arc;

use crate::companion::presence_client::PresenceClient;
use crate::protocol::sequencer::CompanionSequencer;

/// The per-pairing publish authority.
pub struct PublishAuthority {
    pub sequencer: Arc<CompanionSequencer>,
    /// Placeholder at factory time; ALWAYS overwritten by the coordinator
    /// after `resolve` with a brand-new client from the fresh negotiation.
    pub client: Option<Arc<PresenceClient>>,
}

/// Holds at most ONE current authority.
pub struct AuthorityRegistry {
    current: Option<(String, PublishAuthority)>,
}

impl AuthorityRegistry {
    pub fn new() -> Self {
        Self { current: None }
    }

    /// Return the current authority when its key matches, else replace it
    /// with `factory()`'s product and return that. Renegotiation across
    /// sleep/wake reuses the same sequencer so a wake snapshot can never
    /// overtake an in-flight clear's sequence state.
    pub fn resolve(
        &mut self,
        base_url: &str,
        device_id: &str,
        factory: impl FnOnce() -> PublishAuthority,
    ) -> &mut PublishAuthority {
        let key = format!("{base_url}|{device_id}");
        let matches = self
            .current
            .as_ref()
            .is_some_and(|(current_key, _)| *current_key == key);
        if !matches {
            self.current = Some((key, factory()));
        }
        &mut self.current.as_mut().expect("authority just resolved").1
    }

    /// Current authority without resolving, or `None`.
    pub fn peek(&self) -> Option<&PublishAuthority> {
        self.current.as_ref().map(|(_, authority)| authority)
    }

    /// Drop the current authority (next resolve builds a fresh sequencer).
    pub fn discard(&mut self) {
        self.current = None;
    }
}

impl Default for AuthorityRegistry {
    fn default() -> Self {
        Self::new()
    }
}
