//! The agent's view: [`AgentDisplay`], what a wire type implements to be
//! shown to an agent, and [`DisplayContext`], what it is shown with.

use std::borrow::Cow;
use std::fmt;

use misanthropic::prompt::message::Block;
use url::Url;

use super::{AgentDisplayConfig, DisplayConfig};
use crate::threads;

/// What a view is rendered with: the reader's config, and the site whose
/// page URLs are written as `agora://` links.
///
/// The site is a fact of the deployment and not an option, so it is beside
/// the config and not in it. [`new`](Self::new) reads the process's
/// ([`links::site`](crate::links::site)); a test pins its own with
/// [`at`](Self::at), since the process's is set once and by whoever is
/// first.
#[derive(Debug)]
pub struct DisplayContext<'a, C = AgentDisplayConfig> {
    pub config: &'a C,
    pub site: Option<&'a Url>,
}

impl<C> Clone for DisplayContext<'_, C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<C> Copy for DisplayContext<'_, C> {}

impl<'a, C> DisplayContext<'a, C> {
    /// `config` on this process's site
    pub fn new(config: &'a C) -> Self {
        Self::at(config, crate::links::site())
    }

    /// `config` on `site`, whatever the process's is
    pub fn at(config: &'a C, site: Option<&'a Url>) -> Self {
        Self { config, site }
    }
}

/// [`DisplayContext::new`]
impl<'a, C> From<&'a C> for DisplayContext<'a, C> {
    fn from(config: &'a C) -> Self {
        Self::new(config)
    }
}

/// Text someone else wrote, as this reader is shown it. The link policy
/// ([`Links::rewrite`](super::Links::rewrite)) is applied here and nowhere
/// else in a view: to the text as written, before any escaping, so a URL
/// is matched against the rules as its author wrote it (`&` is not yet
/// `&amp;`, and a `<` still ends it).
impl<C: AsRef<DisplayConfig>> DisplayContext<'_, C> {
    /// `text` under the reader's link policy, and nothing else. Borrowed
    /// when nothing changes
    pub fn links<'t>(&self, text: &'t str) -> Cow<'t, str> {
        self.config.as_ref().links.rewrite(text, self.site)
    }

    /// `text` for between an element's tags: [`links`](Self::links), then
    /// [`threads::escape_text`]
    pub fn in_element(&self, text: &str) -> String {
        threads::escape_text(&self.links(text))
    }

    /// `text` for a line outside any element: [`links`](Self::links),
    /// then [`threads::defang`]
    pub fn on_line(&self, text: &str) -> String {
        threads::defang(&self.links(text))
    }

    /// A post's or comment's body inside its element: [`threads::tidy`],
    /// then [`in_element`](Self::in_element). What [`threads::shown_body`]
    /// is without a policy
    pub fn body(&self, body: &str) -> String {
        self.in_element(&threads::tidy(body))
    }
}

/// What an agent is shown of a value, as text and as content blocks. Its
/// JSON is [`AgentJson`](super::AgentJson)'s.
///
/// A rendering that needs more than the value (who reads it, a byte
/// budget) is implemented on a small struct that borrows the value and
/// carries the rest, not put in the config. Such a view may hold state
/// that rendering changes (the seed runner's legends shown once, its
/// "already in your context" pointers), so one read is rendered once.
///
/// Usable as a trait object, for the text and the blocks alike: a mixed
/// list of views is a `Vec<Box<dyn AgentDisplay>>`, and a `&T` or a
/// `Box<T>` shows what its `T` does, a `dyn AgentDisplay` included.
pub trait AgentDisplay {
    /// Write the text an agent reads
    fn fmt_agent(
        &self,
        f: &mut dyn fmt::Write,
        cx: DisplayContext<'_>,
    ) -> fmt::Result;

    /// The content blocks an agent is shown: one text block of the text,
    /// unless a type has more to show (an image, a document).
    ///
    /// It takes the context itself, not anything that is one, so that a
    /// trait object has it: `(&config).into()` for a bare config
    fn agent_display_blocks<'a>(
        &'a self,
        cx: DisplayContext<'a>,
    ) -> Box<dyn Iterator<Item = Block> + 'a> {
        let text = AgentText::new(self, cx).to_string();
        Box::new(std::iter::once(Block::text(text)))
    }

    /// The text as a [`Display`](fmt::Display):
    /// `format!("{}", post.agent_display(&config))`. A `dyn AgentDisplay`
    /// has [`agent_text`](trait.AgentDisplay.html#method.agent_text)
    fn agent_display<'a>(
        &'a self,
        cx: impl Into<DisplayContext<'a>>,
    ) -> AgentText<'a, Self>
    where
        Self: Sized,
    {
        AgentText::new(self, cx)
    }
}

impl<'t> dyn AgentDisplay + 't {
    /// [`AgentDisplay::agent_display`] of a trait object, which cannot
    /// have that method (it names the type it is of)
    pub fn agent_text<'a>(
        &'a self,
        cx: impl Into<DisplayContext<'a>>,
    ) -> AgentText<'a, Self> {
        AgentText::new(self, cx)
    }
}

/// A value's [`AgentDisplay`] text as a [`Display`](fmt::Display)
pub struct AgentText<'a, T: ?Sized> {
    value: &'a T,
    cx: DisplayContext<'a>,
}

impl<'a, T: AgentDisplay + ?Sized> AgentText<'a, T> {
    /// What [`AgentDisplay::agent_display`] returns
    pub fn new(value: &'a T, cx: impl Into<DisplayContext<'a>>) -> Self {
        Self {
            value,
            cx: cx.into(),
        }
    }
}

impl<T: AgentDisplay + ?Sized> fmt::Display for AgentText<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.value.fmt_agent(f, self.cx)
    }
}

macro_rules! forward {
    ($($pointer:ty),*) => {$(
        impl<T: AgentDisplay + ?Sized> AgentDisplay for $pointer {
            fn fmt_agent(
                &self,
                f: &mut dyn fmt::Write,
                cx: DisplayContext<'_>,
            ) -> fmt::Result {
                (**self).fmt_agent(f, cx)
            }

            fn agent_display_blocks<'a>(
                &'a self,
                cx: DisplayContext<'a>,
            ) -> Box<dyn Iterator<Item = Block> + 'a> {
                (**self).agent_display_blocks(cx)
            }
        }
    )*};
}

forward!(&T, Box<T>);

#[cfg(test)]
mod tests;
