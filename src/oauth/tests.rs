use super::*;

#[test]
fn oauth_errors_are_rfc_6749s_shape() {
    assert_eq!(
        serde_json::to_value(OAuthError::new(
            OAuthErrorCode::InvalidGrant,
            "PKCE verification failed"
        ))
        .unwrap(),
        serde_json::json!({
            "error": "invalid_grant",
            "error_description": "PKCE verification failed",
        })
    );
    assert_eq!(
        serde_json::to_value(OAuthError::code(OAuthErrorCode::InvalidClient))
            .unwrap(),
        serde_json::json!({"error": "invalid_client"})
    );
    for (code, text) in [
        (OAuthErrorCode::InvalidRequest, "invalid_request"),
        (
            OAuthErrorCode::UnsupportedGrantType,
            "unsupported_grant_type",
        ),
        (OAuthErrorCode::ServerError, "server_error"),
        (
            OAuthErrorCode::InvalidClientMetadata,
            "invalid_client_metadata",
        ),
        (OAuthErrorCode::InvalidRedirectUri, "invalid_redirect_uri"),
    ] {
        assert_eq!(
            serde_json::to_value(code).unwrap(),
            serde_json::json!(text)
        );
    }
}

#[test]
fn a_token_response_is_rfc_6749s_and_never_debug_prints_a_token() {
    let r = TokenResponse {
        access_token: Secret::new("at-123".into()),
        token_type: Bearer,
        expires_in: 3600,
        refresh_token: Secret::new("rt-456".into()),
        scope: vec![OAuthScope::Read, OAuthScope::Write],
    };
    let json = serde_json::to_value(&r).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "access_token": "at-123",
            "token_type": "Bearer",
            "expires_in": 3600,
            "refresh_token": "rt-456",
            "scope": "read write",
        })
    );
    let debug = format!("{r:?}");
    assert!(
        !debug.contains("at-123") && !debug.contains("rt-456"),
        "{debug}"
    );
    let back: TokenResponse = serde_json::from_value(json).unwrap();
    assert_eq!(back.access_token.expose(), "at-123");
    assert_eq!(back.scope, [OAuthScope::Read, OAuthScope::Write]);
}
