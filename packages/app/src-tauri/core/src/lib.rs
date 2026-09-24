//! yohaku-core — native Rust port of the Node.js companion core
//! (`packages/core`). Runs inside the Tauri v2 process; the React settings UI
//! talks to it via Tauri commands/events instead of the old WebSocket IPC.
//!
//! Non-negotiable compatibility invariants (see
//! `.claude/rewrite/ARCHITECTURE.md`):
//!
//! 1. On-disk formats are unchanged (`config.json` v1, `sequence.json`,
//!    Credential Manager entry layout, DPAPI fallback file,
//!    `YOHAKU_DATA_DIR` override).
//! 2. Companion Protocol v2 bytes are unchanged (endpoints, headers,
//!    envelopes, RFC3339 millisecond UTC dates, 0..2^53-1 integers,
//!    sequence reservation/reconciliation, single idempotent retry with
//!    identical bytes).
//! 3. Privacy: raw capture values (exe paths, un-sanitized titles, media
//!    text) never reach the network, persistence, or logs.
//! 4. UI data shapes serialize to the same camelCase JSON as
//!    `packages/shared/src/*.ts`.
//!
//! Module tree and file ownership are fixed by the architecture contract;
//! `lib.rs`, `model.rs` and every `mod.rs` are scaffold-owned.

pub mod model;

pub mod capture;
pub mod companion;
pub mod privacy;
pub mod protocol;
pub mod runtime;
pub mod store;

pub use model::*;
