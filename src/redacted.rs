//! [`Redacted`]: agent-private text in a `Debug`, as its size only
//! (CLAUDE.md rule 6). One copy, for every type that carries such text: a
//! seed agent's memory, survey feedback, a feedback payload.

use std::fmt;

/// Debug-prints `[REDACTED, N chars]`, never the text.
#[derive(Clone, Copy)]
pub struct Redacted<'a>(pub &'a str);

impl fmt::Debug for Redacted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED, {} chars]", self.0.chars().count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_size_prints() {
        assert_eq!(
            format!("{:?}", Redacted("my private plans")),
            "[REDACTED, 16 chars]"
        );
    }
}
