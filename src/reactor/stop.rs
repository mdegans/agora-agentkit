//! Stopping a run gracefully (agora-agents#293).
//!
//! A [`Stop`] is shared between whoever handles the process's signals and
//! every [`Reactor`](super::Reactor) given it
//! ([`with_stop`](super::Reactor::with_stop)). It only rises:
//!
//! - [`StopLevel::Drain`]: no new session starts. Sessions in flight run to
//!   their end, teardown and save included. An agent whose session hadn't
//!   started stays seated in its reactor, unsaved and untouched, and is
//!   counted in [`Report::unstarted`](super::Report::unstarted).
//! - [`StopLevel::EndTurn`]: as a drain, and each session in flight ends at
//!   its next turn boundary: the turn in flight finishes (its model call
//!   returns, its tool results are seated), then the reactor asks the agent
//!   to close ([`Agent::on_stop`](super::Agent::on_stop), at each boundary
//!   until it does) and the session ends however the agent closes it, saved
//!   as any other. A session that hasn't had its first turn has it first. A
//!   session held on an unreachable endpoint stops holding: it ends in error,
//!   with no closing turns.
//!
//! The reactor only reads the level, at boundaries. Bounding how long a stop
//! may take (a turn can wait on a stuck backend for an hour) is the caller's:
//! the process exits on its own deadline.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// How far a run has been asked to stop. Ordered: each level includes the
/// ones before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum StopLevel {
    /// Not stopping.
    #[default]
    Run,
    /// Start no new session; let those in flight finish.
    Drain,
    /// Start no new session; end those in flight after their current turn.
    EndTurn,
}

impl StopLevel {
    const ALL: [StopLevel; 3] =
        [StopLevel::Run, StopLevel::Drain, StopLevel::EndTurn];

    fn from_u8(n: u8) -> Self {
        Self::ALL[usize::from(n).min(Self::ALL.len() - 1)]
    }

    /// The level's name in logs: `run`, `drain`, `end_turn`.
    pub fn as_str(self) -> &'static str {
        match self {
            StopLevel::Run => "run",
            StopLevel::Drain => "drain",
            StopLevel::EndTurn => "end_turn",
        }
    }
}

impl std::fmt::Display for StopLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A shared, rising [`StopLevel`]. Clones share the level.
#[derive(Debug, Clone, Default)]
pub struct Stop(Arc<AtomicU8>);

impl Stop {
    /// A stop at [`StopLevel::Run`].
    pub fn new() -> Self {
        Self::default()
    }

    /// The current level.
    pub fn level(&self) -> StopLevel {
        StopLevel::from_u8(self.0.load(Ordering::SeqCst))
    }

    /// Raise the level to `level`; a lower one changes nothing. Returns the
    /// level before.
    pub fn request(&self, level: StopLevel) -> StopLevel {
        StopLevel::from_u8(self.0.fetch_max(level as u8, Ordering::SeqCst))
    }

    /// Whether new sessions may start.
    pub fn draining(&self) -> bool {
        self.level() >= StopLevel::Drain
    }

    /// Whether sessions in flight should end at their next turn boundary.
    pub fn ending_turns(&self) -> bool {
        self.level() >= StopLevel::EndTurn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_level_only_rises_and_clones_share_it() {
        let stop = Stop::new();
        let other = stop.clone();
        assert_eq!(stop.level(), StopLevel::Run);
        assert!(!stop.draining());
        assert_eq!(other.request(StopLevel::Drain), StopLevel::Run);
        assert!(stop.draining() && !stop.ending_turns());
        assert_eq!(stop.request(StopLevel::EndTurn), StopLevel::Drain);
        assert_eq!(stop.request(StopLevel::Drain), StopLevel::EndTurn);
        assert_eq!(other.level(), StopLevel::EndTurn);
        assert!(other.ending_turns());
    }
}
