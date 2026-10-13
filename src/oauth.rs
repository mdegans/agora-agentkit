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
use crate::ids::OAuthClientId;
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
/// Not the platform's `{"error": "<message>"}` envelope: `error` is a
/// registered code clients parse, and the prose belongs in
/// `error_description`.
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

/// RFC 8414 authorization server metadata (`/.well-known/oauth-authorization-server`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct OAuthServerMetadata {
    /// Opaque identifier clients match byte-for-byte across requests, so
    /// it is a `String` with trailing slashes trimmed rather than a `Url`:
    /// `Url`'s `Serialize` would re-normalize and re-add the slash,
    /// silently breaking that match.
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: String,
    pub response_types_supported: Vec<String>,
    pub grant_types_supported: Vec<String>,
    pub code_challenge_methods_supported: Vec<String>,
    pub token_endpoint_auth_methods_supported: Vec<String>,
    pub scopes_supported: Vec<String>,
}

/// An RFC 7591 Dynamic Client Registration request.
///
/// Deliberately lenient: RFC 7591 § 2 says the server "MUST ignore any
/// client metadata sent by the client that it does not understand", and
/// MCP clients send plenty (`grant_types`, `logo_uri`, …), so unknown
/// fields are ignored here, unlike every other request body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ClientRegistrationRequest {
    pub redirect_uris: Vec<String>,
    #[serde(default)]
    pub client_name: Option<String>,
    /// `none` (public, PKCE) or `client_secret_post` (confidential); a
    /// string so an unsupported one is refused with RFC 7591's
    /// `invalid_client_metadata`, not a parse error
    #[serde(default = "default_auth_method")]
    pub token_endpoint_auth_method: String,
}

fn default_auth_method() -> String {
    "none".to_string()
}

/// The RFC 7591 § 3.2.1 client information response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ClientRegistrationResponse {
    pub client_id: OAuthClientId,
    /// Issue time, seconds since the Unix epoch
    pub client_id_issued_at: i64,
    pub redirect_uris: Vec<String>,
    pub client_name: String,
    /// `none` (public, PKCE) or `client_secret_post` (confidential)
    pub token_endpoint_auth_method: String,
    pub grant_types: Vec<String>,
    pub response_types: Vec<String>,
    /// Present only for `client_secret_post` clients. A [`Secret`]:
    /// `Debug` prints `[REDACTED]`
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "expose_opt",
        deserialize_with = "secret_opt"
    )]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<String>"))]
    pub client_secret: Option<Secret>,
    /// `0` means the secret never expires (RFC 7591). Present only
    /// alongside `client_secret`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret_expires_at: Option<i64>,
}

/// An OAuth 2.0 token request (`POST /oauth/token`, form-encoded), for
/// either grant: `authorization_code` (a consent-flow code, with PKCE) or
/// `refresh_token` (a rotation; scopes carry through).
///
/// Every credential in it is a [`Secret`], so `Debug` shows which were
/// sent and never what they were, and it has no `Serialize`. `grant_type` is a string so an
/// unsupported one is refused with RFC 6749's `unsupported_grant_type`,
/// not a parse error.
// Deserialize only: a request full of credentials is never written out
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct TokenRequest {
    pub grant_type: String,
    /// `authorization_code`: the code from the consent flow
    #[serde(default, deserialize_with = "secret_opt")]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<String>"))]
    pub code: Option<Secret>,
    #[serde(default)]
    pub redirect_uri: Option<String>,
    /// A confidential client's secret, checked against its stored hash
    #[serde(default, deserialize_with = "secret_opt")]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<String>"))]
    pub client_secret: Option<Secret>,
    /// `authorization_code`: the PKCE verifier
    #[serde(default, deserialize_with = "secret_opt")]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<String>"))]
    pub code_verifier: Option<Secret>,
    /// `refresh_token`: the token to rotate
    #[serde(default, deserialize_with = "secret_opt")]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<String>"))]
    pub refresh_token: Option<Secret>,
    #[serde(default)]
    pub client_id: Option<OAuthClientId>,
}

fn expose_opt<S: Serializer>(
    secret: &Option<Secret>,
    s: S,
) -> Result<S::Ok, S::Error> {
    match secret {
        Some(secret) => s.serialize_some(secret.expose()),
        None => s.serialize_none(),
    }
}

fn secret_opt<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Secret>, D::Error> {
    Ok(Option::<String>::deserialize(d)?.map(Secret::new))
}

#[cfg(test)]
mod tests;
