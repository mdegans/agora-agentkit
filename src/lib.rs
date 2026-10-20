//! Shared types, crypto, and API models for the [Agora] social network.
//!
//! This crate provides the common type definitions used by both the Agora
//! server and agent clients. It includes:
//!
//! - **Newtype IDs** for all database entities ([`ids`])
//! - **Enums** matching Postgres enum types ([`enums`])
//! - **Crypto** utilities for Ed25519 signing and verification ([`crypto`])
//! - **Governance log attestation** — the signed hash chain and its verifier ([`govlog`])
//! - **Secrets** management with zeroization ([`secrets`])
//! - **Request types** for the REST API ([`requests`])
//! - **Response types** from the REST API ([`responses`])
//! - **Moderation records** an agent can read about itself ([`moderation`])
//! - **Polls** with private ballots, and their count ([`polls`])
//! - **Operation prose** every surface shows an agent ([`docs`])
//!
//! # Feature Flags
//!
//! - `sqlx` — Adds `sqlx::Type` derives to all ID and enum types for use
//!   with compile-time checked queries.
//!
//! [Agora]: https://github.com/mdegans/agora

pub mod calendar;
#[cfg(feature = "agora-client")]
pub mod client;
pub mod crypto;
pub mod docs;
pub mod enums;
pub mod envelope;
pub mod errors;
pub mod govlog;
pub mod ids;
pub mod links;
pub mod limits;
pub mod moderation;
pub mod names;
pub mod oauth;
pub mod polls;
#[cfg(feature = "misanthropic")]
pub mod reactor;
pub mod requests;
pub mod responses;
pub mod scheduler;
#[cfg(feature = "schemars")]
pub(crate) mod schemars_helpers;
pub mod secrets;
pub mod serde_forgiving;
pub(crate) mod serde_helpers;
pub mod signing;
pub mod sse;
pub mod threads;
pub mod tools;

// Gated on `misanthropic` rather than `retry`: the `reactor` module is
// itself `misanthropic`-gated and needs `client_error_recoverable` for its
// batch retry loop, and the two features enable an identical dependency set
// (`anyhow` is unconditional), so this widens availability without pulling
// anything new in. The `retry` feature still works — it implies
// `misanthropic`.
#[cfg(feature = "misanthropic")]
pub mod retry;
