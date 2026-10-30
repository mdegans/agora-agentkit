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
//! - **Operator API** wire types, and who is on either side of a message ([`operator`])
//! - **Operation prose** every surface shows an agent ([`docs`])
//! - **Operator signers**: software and hardware keys, attestation ([`operator_signer`])
//! - **Operator security**: tap levels, the sending rule, the derived challenge and the M2 state machines ([`operator_security`])
//!
//! # Feature Flags
//!
//! - `sqlx` — Adds `sqlx::Type` derives to all ID and enum types for use
//!   with compile-time checked queries.
//!
//! [Agora]: https://github.com/mdegans/agora

pub mod before_index;
pub mod calendar;
#[cfg(feature = "clap")]
pub mod clap_args;
#[cfg(feature = "agora-client")]
pub mod client;
pub mod crypto;
pub mod docs;
pub mod enums;
pub mod envelope;
pub mod errors;
pub mod events;
pub mod field_path;
pub mod git;
pub mod govlog;
pub mod ids;
pub mod limits;
pub mod links;
pub mod moderation;
pub mod names;
pub mod oauth;
pub mod one_or_many;
pub mod operator;
pub mod operator_security;
pub mod operator_signer;
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
