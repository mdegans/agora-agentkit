//! An operator's display config for many agents: the default, named
//! profiles, and each agent's or model's choice among them
//! ([`AgentDisplayProfiles`]).
//!
//! A more specific choice **replaces the default whole: nothing is merged
//! between the default, a profile and an inline config.** Of an agent's
//! choice, its model's choice and the default, the first there is applies.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::{AgentDisplayConfig, DisplayConfig, Links};

/// The name of a profile in [`AgentDisplayProfiles`]: 1 to
/// [`MAX_BYTES`](Self::MAX_BYTES) lower-case ASCII letters, digits, `-`
/// and `_`.
///
/// `default` and `profiles` are refused ([`ProfileNameError::Reserved`])
#[derive(
    Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
// Derived as `AgentName`'s is: a plain string, inline, no `pattern`
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(
    feature = "schemars",
    schemars(
        inline,
        description = "A display profile's name: 1 to 64 lower-case ASCII letters, digits, hyphens or underscores; not `default` or `profiles`"
    )
)]
#[serde(try_from = "String", into = "String")]
pub struct ProfileName(String);

impl ProfileName {
    pub const MAX_BYTES: usize = 64;

    /// The words that are not a profile's name. `default` is what
    /// [`DisplaySelection`] reads as the default itself, and what an
    /// operator is told a config is when nothing chose one. `profiles` is
    /// the table's own key: a profile of that name is a table nested once
    /// too often
    pub const RESERVED: [&str; 2] = ["default", "profiles"];

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A string [`ProfileName`] refuses
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileNameError {
    #[error(
        "{given:?} is not a profile name: it is {} bytes, and a name is 1 to {}",
        given.len(),
        ProfileName::MAX_BYTES
    )]
    Length { given: String },
    #[error(
        "{given:?} is not a profile name: it has {character:?}, and a name \
         is lower-case ASCII letters, digits, `-` and `_` only"
    )]
    Character { given: String, character: char },
    #[error("{given:?} is not a profile name: {}", reserved(given))]
    Reserved { given: String },
}

fn reserved(word: &str) -> &'static str {
    if word == DisplaySelection::DEFAULT {
        "it means the default itself, which is the keys of the table the \
         profiles are in"
    } else {
        "it is the table the profiles are in, and a profile is one level \
         inside it"
    }
}

impl FromStr for ProfileName {
    type Err = ProfileNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let given = || s.to_owned();
        if s.is_empty() || s.len() > Self::MAX_BYTES {
            return Err(ProfileNameError::Length { given: given() });
        }
        if let Some(character) = s.chars().find(|c| {
            !(c.is_ascii_lowercase()
                || c.is_ascii_digit()
                || matches!(c, '-' | '_'))
        }) {
            return Err(ProfileNameError::Character {
                given: given(),
                character,
            });
        }
        if Self::RESERVED.contains(&s) {
            return Err(ProfileNameError::Reserved { given: given() });
        }
        Ok(Self(given()))
    }
}

impl TryFrom<String> for ProfileName {
    type Error = ProfileNameError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl TryFrom<&str> for ProfileName {
    type Error = ProfileNameError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<ProfileName> for String {
    fn from(name: ProfileName) -> Self {
        name.0
    }
}

impl AsRef<str> for ProfileName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ProfileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ProfileName({:?})", self.0)
    }
}

impl fmt::Display for ProfileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An operator's display config: the default for every agent, and the
/// named profiles an agent or a model may choose instead
/// ([`DisplaySelection`], [`resolve`](Self::resolve)).
///
/// **A profile replaces the default whole: nothing is merged, so an agent
/// on a profile gets that profile's rules and none of the default's.**
///
/// The default's keys are the table's own, beside `profiles`. An unknown
/// key is refused at every level: a typo must not leave a policy at its
/// default.
///
/// ```toml
/// [display]                      # the default for every agent
/// links = { mode = "defanged" }
///
/// [display.profiles.news]        # a named profile: replaces the default whole
/// links = { mode = "defanged", allow = ["https://*.wikipedia.org", "https://www.bbc.com"] }
///
/// [display.profiles.open]
/// links = { mode = "raw", deny = ["https://example.org/tracker"] }
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "schemars",
    derive(schemars::JsonSchema),
    schemars(inline)
)]
pub struct AgentDisplayProfiles {
    /// The default: what an agent is shown text with when neither it nor
    /// its model chose a profile
    // As in `AgentDisplayConfig`: the keys that are not `profiles` are the
    // default's, which refuses the ones it does not know
    #[serde(flatten, deserialize_with = "strict_default")]
    pub default: AgentDisplayConfig,
    /// The named profiles. One chosen for an agent or a model replaces
    /// the default whole: nothing is merged, so list in a profile
    /// everything its agents should have
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "named_profiles"
    )]
    pub profiles: BTreeMap<ProfileName, AgentDisplayConfig>,
}

/// Every key the flattening struct did not take, read as the default
fn strict_default<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<AgentDisplayConfig, D::Error> {
    let rest = serde_json::Map::<String, serde_json::Value>::deserialize(d)?;
    AgentDisplayConfig::deserialize(serde_json::Value::Object(rest))
        .map_err(serde::de::Error::custom)
}

/// The profiles, an error in one naming it
fn named_profiles<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<ProfileName, AgentDisplayConfig>, D::Error> {
    BTreeMap::<ProfileName, serde_json::Value>::deserialize(d)?
        .into_iter()
        .map(
            |(name, value)| match AgentDisplayConfig::deserialize(value) {
                Ok(config) => Ok((name, config)),
                Err(e) => Err(serde::de::Error::custom(format_args!(
                    "profile `{name}`: {e}"
                ))),
            },
        )
        .collect()
}

/// What an agent or a model has in place of the default: a profile by
/// name (`display = "news"`), a config written in place
/// (`display = { links = { mode = "raw" } }`), or the default itself by
/// its name (`display = "default"`, for an agent whose model chose a
/// profile).
///
/// **It replaces the default whole: nothing is merged, so a config written
/// in place has only the rules written in it.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplaySelection {
    /// [`AgentDisplayProfiles::default`], whatever a less specific level
    /// chose
    Default,
    Profile(ProfileName),
    Inline(AgentDisplayConfig),
}

impl DisplaySelection {
    /// What [`Default`](Self::Default) is written as. Never a profile's
    /// name ([`ProfileName::RESERVED`])
    pub const DEFAULT: &str = "default";
}

impl From<ProfileName> for DisplaySelection {
    fn from(name: ProfileName) -> Self {
        Self::Profile(name)
    }
}

impl From<AgentDisplayConfig> for DisplaySelection {
    fn from(config: AgentDisplayConfig) -> Self {
        Self::Inline(config)
    }
}

/// `default`, or a [`ProfileName`]
impl FromStr for DisplaySelection {
    type Err = ProfileNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == Self::DEFAULT {
            Ok(Self::Default)
        } else {
            s.parse().map(Self::Profile)
        }
    }
}

impl Serialize for DisplaySelection {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Default => s.serialize_str(Self::DEFAULT),
            Self::Profile(name) => name.serialize(s),
            Self::Inline(config) => config.serialize(s),
        }
    }
}

// By hand, not `untagged`: its error names neither the value nor what was
// wrong with it ("data did not match any variant")
impl<'de> Deserialize<'de> for DisplaySelection {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Self, D::Error> {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = DisplaySelection;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    "a display profile's name (\"news\"), \"default\", or \
                     a display config written in place \
                     ({ links = { mode = \"raw\" } })",
                )
            }

            fn visit_str<E: serde::de::Error>(
                self,
                s: &str,
            ) -> Result<Self::Value, E> {
                s.parse().map_err(E::custom)
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<Self::Value, A::Error> {
                AgentDisplayConfig::deserialize(
                    serde::de::value::MapAccessDeserializer::new(map),
                )
                .map(DisplaySelection::Inline)
            }
        }

        d.deserialize_any(Visitor)
    }
}

// A name or a config, both inline: never a `$ref` (agora CLAUDE.md)
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for DisplaySelection {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("DisplaySelection")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(
            module_path!(),
            "::DisplaySelection"
        ))
    }

    fn json_schema(
        generator: &mut schemars::SchemaGenerator,
    ) -> schemars::Schema {
        let config = generator.subschema_for::<AgentDisplayConfig>();
        schemars::json_schema!({
            "description": "What this agent or model is shown text with, \
                in place of the default: a display profile's name, \
                \"default\", or a display config written in place. It \
                replaces the default whole: nothing is merged",
            "anyOf": [
                {
                    "type": "string",
                    "description": "A display profile's name, or \
                        \"default\" for the default whatever a less \
                        specific level chose",
                },
                config,
            ],
        })
    }
}

/// A [`DisplaySelection`] names a profile there is not
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownProfile {
    pub name: ProfileName,
    /// Every profile there is, in order
    pub known: Vec<ProfileName>,
}

impl std::error::Error for UnknownProfile {}

impl fmt::Display for UnknownProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "there is no display profile `{}`: ", self.name)?;
        match self.known.split_first() {
            None => f.write_str("no profile is defined"),
            Some((first, rest)) => {
                write!(f, "the profiles are `{first}`")?;
                rest.iter().try_for_each(|name| write!(f, ", `{name}`"))
            }
        }
    }
}

/// Which of an agent and its model made a [`DisplaySelection`]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelectedBy {
    Model,
    Agent,
}

/// The config an agent is shown text with, and where it came from. Its
/// [`Display`](fmt::Display) is the description an operator reads.
///
/// ```text
/// display: profile `news`, chosen for the agent
///   in place of the default, whole: nothing of the default is added to it
///   not used: the model's choice, profile `open`
/// links: defanged, but for 2 allowed
///   allow https://*.wikipedia.org
///   allow https://www.bbc.com
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedDisplay<'a> {
    pub config: &'a AgentDisplayConfig,
    /// `None` when nothing was chosen: the default
    pub chosen: Option<(SelectedBy, &'a DisplaySelection)>,
    /// The model's selection, when the agent's was taken over it
    pub passed_over: Option<&'a DisplaySelection>,
}

impl AgentDisplayProfiles {
    /// The config of an agent with these selections: the agent's own if it
    /// has one, else its model's, else the default.
    ///
    /// **The one chosen is taken whole: nothing is merged with the default
    /// or with a less specific selection.** An unknown profile is an error
    /// only in the selection that applies: [`check`](Self::check) each one
    /// when the config is loaded.
    pub fn resolve<'a>(
        &'a self,
        model: Option<&'a DisplaySelection>,
        agent: Option<&'a DisplaySelection>,
    ) -> Result<ResolvedDisplay<'a>, UnknownProfile> {
        let chosen = match (agent, model) {
            (Some(agent), _) => Some((SelectedBy::Agent, agent)),
            (None, Some(model)) => Some((SelectedBy::Model, model)),
            (None, None) => None,
        };
        Ok(ResolvedDisplay {
            config: match chosen {
                Some((_, selection)) => self.selected(selection)?,
                None => &self.default,
            },
            chosen,
            passed_over: agent.and(model),
        })
    }

    /// Whether `selection` can be resolved here. For every selection in a
    /// config when it is loaded, so that a misspelt profile is found at
    /// startup and not when its agent first runs
    pub fn check(
        &self,
        selection: &DisplaySelection,
    ) -> Result<(), UnknownProfile> {
        self.selected(selection).map(|_| ())
    }

    fn selected<'a>(
        &'a self,
        selection: &'a DisplaySelection,
    ) -> Result<&'a AgentDisplayConfig, UnknownProfile> {
        match selection {
            DisplaySelection::Default => Ok(&self.default),
            DisplaySelection::Inline(config) => Ok(config),
            DisplaySelection::Profile(name) => {
                self.profiles.get(name).ok_or_else(|| UnknownProfile {
                    name: name.clone(),
                    known: self.profiles.keys().cloned().collect(),
                })
            }
        }
    }
}

/// A selection as an operator reads it: "profile `news`"
struct Named<'a>(&'a DisplaySelection);

impl fmt::Display for Named<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            DisplaySelection::Default => f.write_str("the default, by name"),
            DisplaySelection::Profile(name) => write!(f, "profile `{name}`"),
            DisplaySelection::Inline(_) => {
                f.write_str("a config written in place")
            }
        }
    }
}

/// Plain text, a line per fact and per rule (every rule: nothing is left
/// out), each ended by a newline. Stable: a change to it is a change an
/// operator's scripts may see
impl fmt::Display for ResolvedDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.chosen {
            None => writeln!(
                f,
                "display: the default (nothing is chosen for the agent or \
                 its model)"
            )?,
            Some((by, selection)) => {
                let by = match by {
                    SelectedBy::Model => "chosen by the model",
                    SelectedBy::Agent => "chosen for the agent",
                };
                writeln!(f, "display: {}, {by}", Named(selection))?;
                if *selection != DisplaySelection::Default {
                    writeln!(
                        f,
                        "  in place of the default, whole: nothing of the \
                         default is added to it"
                    )?;
                }
            }
        }
        if let Some(model) = self.passed_over {
            writeln!(f, "  not used: the model's choice, {}", Named(model))?;
        }
        // Every option, so one added to the config is described here or
        // does not compile
        let AgentDisplayConfig {
            common: DisplayConfig { links },
        } = self.config;
        match links {
            Links::Defanged {
                allow,
                allow_authors,
            } => {
                let authors = match allow_authors.len() {
                    0 => String::new(),
                    1 => " in 1 author's own text".to_string(),
                    n => format!(" in {n} authors' own text"),
                };
                match allow.len() {
                    0 if authors.is_empty() => {
                        writeln!(f, "links: defanged, every off-site URL")?
                    }
                    0 => writeln!(
                        f,
                        "links: defanged, every off-site URL but{authors}"
                    )?,
                    n if authors.is_empty() => {
                        writeln!(f, "links: defanged, but for {n} allowed")?
                    }
                    n => writeln!(
                        f,
                        "links: defanged, but for {n} allowed, and{authors}"
                    )?,
                }
                allow.iter().try_for_each(|r| writeln!(f, "  allow {r}"))?;
                allow_authors
                    .iter()
                    .try_for_each(|name| writeln!(f, "  author {name}"))
            }
            Links::Raw { deny } if deny.is_empty() => {
                writeln!(f, "links: raw, every URL left as written")
            }
            Links::Raw { deny } => {
                writeln!(
                    f,
                    "links: raw (left as written), but for {} denied",
                    deny.len()
                )?;
                deny.iter().try_for_each(|r| writeln!(f, "  deny {r}"))
            }
        }
    }
}

#[cfg(test)]
mod tests;
