//! Companion Protocol v2 — pure wire logic (no I/O).
//!
//! Scaffold-owned mod file. Module bodies are owned by agent P.

pub mod capabilities;
pub mod dto_mapper;
pub mod sequencer;
pub mod types;
pub mod wire;

pub use capabilities::{
    compare_semantic_versions, negotiate_presence, parse_semantic_version, PresenceNegotiation,
    SemanticVersion,
};
pub use dto_mapper::{make_clear_request, make_presence_request, MakeRequestOptions, MappedRequest};
pub use sequencer::{CompanionSequencer, SequenceBacking, SequencerError};
pub use wire::{
    decode_wire_date, encode_wire_date, is_valid_wire_identifier, WireError,
    MAXIMUM_SAFE_WIRE_INTEGER, PRESENCE_SCHEMA, PRESENCE_SCHEMA_VERSION, PROTOCOL_CLIENT_VERSION,
};
