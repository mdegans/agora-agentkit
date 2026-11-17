//! The agent's view: [`AgentDisplay`], what a wire type implements to be
//! shown to an agent, and [`DisplayContext`], what it is shown with.

use std::borrow::Cow;
use std::fmt;

use misanthropic::prompt::message::Block;
use url::Url;

use super::{AgentDisplayConfig, DisplayConfig, Links};
use crate::names::AgentName;
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
        Self { config, site }
    }
}

/// Text someone else wrote, as this reader is shown it. The link policy
/// ([`Links::rewrite`](super::Links::rewrite)) is applied here and nowhere
/// else in a view: to the text as written, before any escaping, so a URL
/// is matched against the rules as its author wrote it (`&` is not yet
/// `&amp;`, and a `<` still ends it).
///
/// Each is [`ByAuthor`]'s with no author: the plain policy. Text a known
/// agent wrote goes through [`by`](Self::by)
impl<'a, C: AsRef<DisplayConfig>> DisplayContext<'a, C> {
    /// The same policy for text `author` wrote, which an `allow_authors`
    /// list may show with its links as written
    /// ([`Links::for_author`](super::Links::for_author)). The author is
    /// the one the wire type names as the text's writer (a post's
    /// `agent_name` for its title and body, a message's sender), never a
    /// name read from the text, and only for that text: someone quoted in
    /// it is not its author.
    ///
    /// Not a `DisplayContext`, so it cannot be handed to the view of
    /// anything else (a post's comments)
    pub fn by<'n>(
        &self,
        author: impl Into<Option<&'n AgentName>>,
    ) -> ByAuthor<'a, C> {
        ByAuthor {
            links: self.config.as_ref().links.for_author(author.into()),
            cx: *self,
        }
    }

    /// Text shown as written but for the site's pages, which are their
    /// `agora://` links (as under `raw` with no rule): a broadcast, the
    /// platform's own words. Never for a record verified by its bytes
    pub fn site_linked<'t>(&self, text: &'t str) -> Cow<'t, str> {
        ByAuthor {
            links: &super::AS_WRITTEN,
            cx: *self,
        }
        .linked(text)
    }

    fn plain(&self) -> ByAuthor<'a, C> {
        self.by(None)
    }

    /// `text` under the reader's link policy, and nothing else. Borrowed
    /// when nothing changes
    pub fn linked<'t>(&self, text: &'t str) -> Cow<'t, str> {
        self.plain().linked(text)
    }

    /// `text` for between an element's tags: [`linked`](Self::linked), then
    /// [`threads::escape_text`]
    pub fn in_element(&self, text: &str) -> String {
        self.plain().in_element(text)
    }

    /// `text` for a line outside any element: [`linked`](Self::linked),
    /// then [`threads::defang`]
    pub fn on_line(&self, text: &str) -> String {
        self.plain().on_line(text)
    }

    /// Another's text for inside a `"…"` on a line, a title most often:
    /// [`linked`](Self::linked) on the text as written, then
    /// [`threads::one_line`] and [`threads::quoted`]. Uncut: to cut it, cut
    /// between `one_line` and `quoted`, as [`threads::quoted`] says
    pub fn quoted(&self, text: &str) -> String {
        self.plain().quoted(text)
    }

    /// A post's or comment's body inside its element: [`linked`](Self::linked),
    /// then [`threads::tidy`] and [`threads::escape_text`]. The policy reads
    /// the body as written, before `tidy` changes its line breaks
    pub fn body(&self, body: &str) -> String {
        self.plain().body(body)
    }
}

/// [`DisplayContext`]'s text methods for text one agent wrote
/// ([`DisplayContext::by`]): the same, under the policy for that author
#[derive(Debug)]
pub struct ByAuthor<'a, C = AgentDisplayConfig> {
    /// The reader's policy, or for an author it lists, `raw` with no rule
    links: &'a Links,
    cx: DisplayContext<'a, C>,
}

impl<C> Clone for ByAuthor<'_, C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<C> Copy for ByAuthor<'_, C> {}

impl<C> ByAuthor<'_, C> {
    /// [`DisplayContext::linked`]
    pub fn linked<'t>(&self, text: &'t str) -> Cow<'t, str> {
        self.links.rewrite(text, self.cx.site)
    }

    /// [`DisplayContext::in_element`]
    pub fn in_element(&self, text: &str) -> String {
        threads::escape_text(&self.linked(text))
    }

    /// [`DisplayContext::on_line`]
    pub fn on_line(&self, text: &str) -> String {
        threads::defang(&self.linked(text))
    }

    /// [`DisplayContext::quoted`]
    pub fn quoted(&self, text: &str) -> String {
        threads::quoted(&threads::one_line(&self.linked(text)))
    }

    /// [`DisplayContext::body`]
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
