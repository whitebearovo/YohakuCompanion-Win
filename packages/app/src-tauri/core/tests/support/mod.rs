//! Shared support for the yohaku-core integration test crates.
//!
//! Each test crate that needs the mock Companion server declares
//! `mod support;` and uses `support::mock_server::*`. Owned by agent T;
//! consumed by tests/presence_client.rs (T) and tests/integration.rs (K).
#![allow(dead_code)]

pub mod mock_server;
