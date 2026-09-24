//! Privacy pipeline: rules/mappings model, decision engine, sanitizers,
//! policy fingerprint, capture orchestration and media session identity.
//!
//! Scaffold-owned mod file. Module bodies are owned by agent V.
//! Security invariant: the sanitized types (in `crate::model`) are the ONLY
//! shapes allowed to flow toward the network, the preview UI, or
//! persistence; raw capture types exist only between the capture layer and
//! this pipeline.

pub mod capture_service;
pub mod evaluator;
pub mod fingerprint;
pub mod media_session_tracker;
pub mod model;
pub mod sanitize;

pub use capture_service::{CaptureOptions, CaptureService, MEDIA_TIMEOUT_MS};
pub use evaluator::{media_decision, process_decision, MediaDecision, ProcessDecision};
pub use fingerprint::policy_fingerprint;
pub use media_session_tracker::{MediaSemanticIdentity, MediaSessionTracker};
pub use sanitize::{
    sanitize_application, sanitize_media, ApplicationSanitizeInput, MediaSanitizeInput,
    SanitizeMediaOptions, SanitizedMediaWithoutSession,
};
