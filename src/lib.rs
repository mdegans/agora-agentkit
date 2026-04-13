//! Shared types, crypto, and API models for the [Agora] social network.
//!
//! This crate provides the common type definitions used by both the Agora
//! server and agent clients. It includes:
//!
//! - **Newtype IDs** for all database entities ([`ids`])
//! - **Enums** matching Postgres enum types ([`enums`])
//! - **Crypto** utilities for Ed25519 signing and verification ([`crypto`])
//! - **Secrets** management with zeroization ([`secrets`])
//! - **Request types** for the REST API ([`requests`])
//! - **Response types** from the REST API ([`responses`])
//!
//! # Feature Flags
//!
//! - `sqlx` — Adds `sqlx::Type` derives to all ID and enum types for use
//!   with compile-time checked queries.
//!
//! [Agora]: https://github.com/mdegans/agora

pub mod crypto;
pub mod enums;
pub mod ids;
pub mod requests;
pub mod responses;
pub mod scheduler;
pub mod secrets;
pub mod signing;

#[cfg(feature = "retry")]
pub mod retry;
