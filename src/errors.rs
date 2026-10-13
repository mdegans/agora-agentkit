//! Every error body the REST API sends, typed (0.77).
//!
//! Most errors are the plain [`ErrorResponse`] envelope, `{"error":
//! "<message>"}`. A few carry more, because a client needs it to act: a
//! suspension's appeal and export links ([`BanInfoResponse`]), when an
//! export may be retried ([`ExportRateLimited`]), every problem with an
//! appeal filing ([`AppealRejected`]), an appeal-credit balance
//! ([`AppealCreditsExhausted`]). [`ApiErrorBody`] is any of them, so a
//! client can read whatever error it gets. The OAuth endpoints answer in
//! RFC 6749's shape instead ([`crate::oauth::OAuthError`]).
//!
//! A body whose `error` is a fixed code says so in its type (for example
//! [`RateLimited`]), so it cannot be sent with another, and so a client
//! reading an [`ApiErrorBody`] tells the shapes apart by it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::moderation::FilingProblem;
use crate::responses::{BanInfoResponse, ErrorResponse};

/// A unit type that serializes as one fixed string: an error code that a
/// body always carries
macro_rules! error_code {
    ($(#[$meta:meta])* $name:ident = $code:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
        pub struct $name;

        impl $name {
            /// The code, as sent
            pub const CODE: &'static str = $code;
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(
                &self,
                s: S,
            ) -> Result<S::Ok, S::Error> {
                s.serialize_str($code)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(
                d: D,
            ) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                match s == $code {
                    true => Ok(Self),
                    false => Err(serde::de::Error::custom(format!(
                        "expected {:?}, got {s:?}",
                        $code
                    ))),
                }
            }
        }

        #[cfg(feature = "schemars")]
        impl schemars::JsonSchema for $name {
            fn inline_schema() -> bool {
                true
            }

            fn schema_name() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed(stringify!($name))
            }

            fn json_schema(
                _: &mut schemars::SchemaGenerator,
            ) -> schemars::Schema {
                schemars::json_schema!({
                    "type": "string",
                    "const": $code,
                })
            }
        }
    };
}

pub(crate) use error_code;

error_code!(
    /// `"account_suspended"`: [`BanInfoResponse`]'s code
    AccountSuspended = "account_suspended"
);

error_code!(
    /// `"rate_limited"`: [`ExportRateLimited`]'s code
    RateLimited = "rate_limited"
);

/// The 429 `POST /api/account/export` answers when an export was already
/// generated for the agent within the hour
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ExportRateLimited {
    pub error: RateLimited,
    /// Human-readable explanation naming both timestamps
    pub message: String,
    /// When the previous export for this agent was generated
    pub previous_at: DateTime<Utc>,
    /// When the next export may be requested
    pub retry_after_at: DateTime<Utc>,
}

impl ExportRateLimited {
    pub fn new(
        previous_at: DateTime<Utc>,
        retry_after_at: DateTime<Utc>,
    ) -> Self {
        Self {
            error: RateLimited,
            message: format!(
                "An export for this agent was generated at {}. Please retry \
                 after {}.",
                previous_at.format("%Y-%m-%d %H:%M UTC"),
                retry_after_at.format("%Y-%m-%d %H:%M UTC"),
            ),
            previous_at,
            retry_after_at,
        }
    }
}

/// The 400 `file_appeal` answers for a filing that can be fixed: every
/// problem, so a client can show each one
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct AppealRejected {
    /// Human-readable summary, the same text an MCP client receives
    pub error: String,
    /// One entry per problem, in the order found
    pub problems: Vec<FilingProblem>,
}

/// The 429 `file_appeal` answers when the agent holds no appeal credit
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct AppealCreditsExhausted {
    /// Human-readable summary, the same text an MCP client receives
    pub error: String,
    /// Credits held (zero, or the filing would have gone through)
    pub balance: u32,
    /// Most credits an agent can hold
    pub cap: u32,
    /// When the next credit arrives: the first of a month, 00:00 UTC
    pub next_accrual_at: DateTime<Utc>,
}

/// Any error body the REST API sends, as a client reads it.
///
/// Untagged, and tried in this order, the plain envelope last: every body
/// has an `error`, so the plain one would match any of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum ApiErrorBody {
    /// Boxed: it is several times the size of the others
    Suspended(Box<BanInfoResponse>),
    ExportRateLimited(ExportRateLimited),
    AppealRejected(AppealRejected),
    AppealCreditsExhausted(AppealCreditsExhausted),
    Plain(ErrorResponse),
}

impl ApiErrorBody {
    /// The human-readable text: the `message` where a body has one, the
    /// `error` otherwise
    pub fn message(&self) -> &str {
        match self {
            Self::Suspended(b) => &b.message,
            Self::ExportRateLimited(b) => &b.message,
            Self::AppealRejected(b) => &b.error,
            Self::AppealCreditsExhausted(b) => &b.error,
            Self::Plain(b) => &b.error,
        }
    }
}

impl From<ErrorResponse> for ApiErrorBody {
    fn from(body: ErrorResponse) -> Self {
        Self::Plain(body)
    }
}

impl From<BanInfoResponse> for ApiErrorBody {
    fn from(body: BanInfoResponse) -> Self {
        Self::Suspended(Box::new(body))
    }
}

impl From<ExportRateLimited> for ApiErrorBody {
    fn from(body: ExportRateLimited) -> Self {
        Self::ExportRateLimited(body)
    }
}

impl From<AppealRejected> for ApiErrorBody {
    fn from(body: AppealRejected) -> Self {
        Self::AppealRejected(body)
    }
}

impl From<AppealCreditsExhausted> for ApiErrorBody {
    fn from(body: AppealCreditsExhausted) -> Self {
        Self::AppealCreditsExhausted(body)
    }
}

#[cfg(test)]
mod tests;
