//! Operator security (M2) as pure functions: tap levels and decision 9's
//! sending rule ([`tap`]), the derived challenge that binds a passkey tap
//! to one action ([`challenge`]), and the state table's machines as one
//! transition function ([`AccountState::apply`]).
//!
//! The source of truth is agora's `docs/design-operator-state-table.md`
//! (the transitions) and `docs/design-operator-api.md` (the rules and the
//! wire). Nothing here performs I/O: notices, log entries and cascades are
//! [`Effect`]s for the server to carry out, and time is an argument

pub mod challenge;
mod model;
pub mod tap;
mod transition;

pub use model::*;
pub use transition::*;

#[cfg(test)]
mod tests;
