//! The OAuth 2.0 endpoints' wire types (0.77): the token response and the
//! error body.
//!
//! The clients that read them are MCP clients (Claude.ai, ChatGPT), which
//! do not link agentkit. They live here anyway because they are part of
//! the public API, and the API's types have one copy (agora CLAUDE.md
//! rule 8).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::enums::OAuthScope;
use crate::errors::error_code;
use crate::secrets::Secret;

/// An OAuth 2.0 error code (RFC 6749 § 5.2, RFC 7591 § 3.2.2), the ones
/// Agora's endpoints send
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum OAuthErrorCode {
    InvalidRequest,
    InvalidClient,
    InvalidGrant,
    UnsupportedGrantType,
    ServerError,
    /// RFC 7591: the registration's metadata is invalid
    InvalidClientMetadata,
    /// RFC 7591: a redirect URI is invalid
    InvalidRedirectUri,
}

/// The OAuth endpoints' error body (RFC 6749 § 5.2, reused by RFC 7591
/// § 3.2.2).
///
/// Not the platform's [`ErrorResponse`](crate::responses::ErrorResponse)
/// envelope: `error` is a registered code clients parse, and the prose
/// belongs in `error_description`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct OAuthError {
    pub error: OAuthErrorCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_description: Option<String>,
}

impl OAuthError {
    pub fn new(error: OAuthErrorCode, description: impl Into<String>) -> Self {
        Self {
            error,
            error_description: Some(description.into()),
        }
    }

    /// An error with only its code
    pub fn code(error: OAuthErrorCode) -> Self {
        Self {
            error,
            error_description: None,
        }
    }
}

error_code!(
    /// `"Bearer"`: the only token type Agora issues
    Bearer = "Bearer"
);

/// A successful token response (RFC 6749 § 5.1), from either grant.
///
/// Both tokens are [`Secret`]s: `Debug` and `Display` print
/// `[REDACTED]`, and they are serialized only here, for the response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct TokenResponse {
    #[serde(serialize_with = "expose", deserialize_with = "secret")]
    #[cfg_attr(feature = "schemars", schemars(with = "String"))]
    pub access_token: Secret,
    pub token_type: Bearer,
    /// Seconds until the access token expires
    pub expires_in: u64,
    #[serde(serialize_with = "expose", deserialize_with = "secret")]
    #[cfg_attr(feature = "schemars", schemars(with = "String"))]
    pub refresh_token: Secret,
    /// The scopes granted, space-separated (RFC 6749 § 3.3)
    #[serde(serialize_with = "join_scopes", deserialize_with = "split_scopes")]
    #[cfg_attr(feature = "schemars", schemars(with = "String"))]
    pub scope: Vec<OAuthScope>,
}

fn expose<S: Serializer>(secret: &Secret, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(secret.expose())
}

fn secret<'de, D: Deserializer<'de>>(d: D) -> Result<Secret, D::Error> {
    String::deserialize(d).map(Secret::new)
}

fn join_scopes<S: Serializer>(
    scopes: &[OAuthScope],
    s: S,
) -> Result<S::Ok, S::Error> {
    let words: Vec<String> = scopes.iter().map(ToString::to_string).collect();
    s.serialize_str(&words.join(" "))
}

fn split_scopes<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Vec<OAuthScope>, D::Error> {
    String::deserialize(d)?
        .split_whitespace()
        .map(|w| w.parse().map_err(serde::de::Error::custom))
        .collect()
}

#[cfg(test)]
mod tests;
