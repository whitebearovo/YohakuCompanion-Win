//! Capture layer: raw foreground/media/system-event sources.
//!
//! Scaffold-owned mod file — it also hosts the cross-module capture TRAITS
//! (platform-neutral contract consumed by the privacy pipeline and the app
//! crate) so they stay stable while the Windows-gated implementations are
//! filled in. Ownership: agent C owns {foreground, system_events} (+
//! `runtime::suspend_detector`); agent M owns {media}.
//!
//! The raw data types ([`ForegroundInfo`], [`MediaSnapshot`]) live in
//! `crate::model` and are re-exported here; they must never reach the
//! network, persistence, or the UI without passing the sanitizers.

use std::time::Duration;

use async_trait::async_trait;

use crate::model::Unsubscribe;

pub use crate::model::{ForegroundInfo, MediaKind, MediaSnapshot};

#[cfg(windows)]
pub mod foreground;
#[cfg(windows)]
pub mod media;
#[cfg(windows)]
pub mod system_events;

#[cfg(windows)]
pub use foreground::ForegroundWatcher;
#[cfg(windows)]
pub use media::{select_media_provider, MediaProviderError, WinRtMediaProvider};
#[cfg(windows)]
pub use system_events::SystemEvents;

/// Synchronous foreground lookup consumed by the privacy `CaptureService`
/// (a FRESH sample per call, with stale-fallback semantics inside the
/// implementation). Implemented by `ForegroundWatcher`.
pub trait ForegroundSource: Send + Sync {
    fn current(&self) -> Option<ForegroundInfo>;
}

/// Media provider contract (single WinRT implementation; the trait exists
/// for test fakes and to keep the privacy pipeline platform-neutral).
///
/// Providers are constructed started (the TS `start()` is subsumed by the
/// async constructor — scaffold decision, see integration-notes).
#[async_trait]
pub trait MediaProvider: Send + Sync {
    /// `"winrt"` for the production provider (feeds
    /// `MediaProviderHealth.kind`).
    fn kind(&self) -> &'static str;

    /// Bounded fresh lookup of the current session; `None` when nothing
    /// plays, on timeout, or on any provider error (errors never
    /// propagate).
    async fn get_snapshot(&self, timeout: Duration) -> Option<MediaSnapshot>;

    /// Fires on semantic changes only (source app / title / artist / album /
    /// play-pause / session appear-disappear), NEVER on position ticks.
    fn on_semantic_change(&self, callback: Box<dyn Fn() + Send + Sync>) -> Unsubscribe;

    fn healthy(&self) -> bool;

    /// After stop: `healthy()` is false and no events fire.
    async fn stop(&self);
}
