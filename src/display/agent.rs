//! The agent's view: [`AgentDisplay`], what a wire type implements to be
//! shown to an agent, and [`DisplayContext`], what it is shown with.

use std::borrow::Cow;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use misanthropic::prompt::message::Block;
use url::Url;

use super::{AgentDisplayConfig, DisplayConfig};
use crate::threads;

/// What a view is rendered with: the reader's config, and the site whose
/// page URLs are written as `agora://` links.
///
/// The site is a fact of the deployment and not an option, so it is beside
/// the config and not in it: the client's base URL
/// ([`Client::site`](crate::client::Client::site)) for a runner.
#[derive(Debug)]
pub struct DisplayContext<'a, C = AgentDisplayConfig> {
    pub config: &'a C,
    pub site: Option<&'a Url>,
    /// Set when the policy defanged something ([`noting`](Self::noting))
    defanged: Option<&'a AtomicBool>,
}

impl<C> Clone for DisplayContext<'_, C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<C> Copy for DisplayContext<'_, C> {}

impl<'a, C> DisplayContext<'a, C> {
    /// `config` on `site`
    pub fn at(config: &'a C, site: Option<&'a Url>) -> Self {
        Self {
            config,
            site,
            defanged: None,
        }
    }

    /// The same, setting `flag` whenever [`linked`](Self::linked) defangs
    /// a URL, so a reader can be told once what one is
    pub fn noting(self, flag: &'a AtomicBool) -> Self {
        Self {
            defanged: Some(flag),
            ..self
        }
    }

    /// Whether the flag of [`noting`](Self::noting) is set
    pub fn has_defanged(&self) -> bool {
        self.defanged.is_some_and(|f| f.load(Ordering::Relaxed))
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
    pub fn linked<'t>(&self, text: &'t str) -> Cow<'t, str> {
        let (text, defanged) =
            self.config.as_ref().links.rewrite_noting(text, self.site);
        if defanged && let Some(flag) = self.defanged {
            flag.store(true, Ordering::Relaxed);
        }
        text
    }

    /// `text` for between an element's tags: [`linked`](Self::linked), then
    /// [`threads::escape_text`]
    pub fn in_element(&self, text: &str) -> String {
        threads::escape_text(&self.linked(text))
    }

    /// `text` for a line outside any element: [`linked`](Self::linked),
    /// then [`threads::defang`]
    pub fn on_line(&self, text: &str) -> String {
        threads::defang(&self.linked(text))
    }

    /// A post's or comment's body inside its element: [`linked`](Self::linked),
    /// then [`threads::tidy`] and [`threads::escape_text`]. The policy reads
    /// the body as written, before `tidy` changes its line breaks
    pub fn body(&self, body: &str) -> String {
        threads::escape_text(&threads::tidy(&self.linked(body)))
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
    /// A trait object has it, as it takes the context itself
    fn agent_display_blocks<'a>(
        &'a self,
        cx: DisplayContext<'a>,
    ) -> Box<dyn Iterator<Item = Block> + 'a> {
        let text = AgentText::new(self, cx).to_string();
        Box::new(std::iter::once(Block::text(text)))
    }

    /// The text as a [`Display`](fmt::Display):
    /// `format!("{}", post.agent_display(cx))`. A `dyn AgentDisplay`
    /// has [`agent_text`](trait.AgentDisplay.html#method.agent_text)
    fn agent_display<'a>(
        &'a self,
        cx: DisplayContext<'a>,
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
        cx: DisplayContext<'a>,
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
    pub fn new(value: &'a T, cx: DisplayContext<'a>) -> Self {
        Self { value, cx }
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
