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

#[test]
fn requests_and_registrations_never_debug_print_a_credential() {
    let r: TokenRequest = serde_json::from_value(serde_json::json!({
        "grant_type": "authorization_code",
        "code": "c-1",
        "code_verifier": "v-2",
        "client_secret": "s-3",
        "redirect_uri": "https://example.org/cb",
    }))
    .unwrap();
    let debug = format!("{r:?}");
    for credential in ["c-1", "v-2", "s-3"] {
        assert!(!debug.contains(credential), "{debug}");
    }
    assert_eq!(r.code.as_ref().map(Secret::expose), Some("c-1"));
    assert!(r.refresh_token.is_none());

    let registered = ClientRegistrationResponse {
        client_id: "c".parse().unwrap(),
        client_id_issued_at: 0,
        redirect_uris: vec![],
        client_name: "n".into(),
        token_endpoint_auth_method: "client_secret_post".into(),
        grant_types: vec![],
        response_types: vec![],
        client_secret: Some(Secret::new("s-4".into())),
        client_secret_expires_at: Some(0),
    };
    assert!(!format!("{registered:?}").contains("s-4"));
    assert_eq!(
        serde_json::to_value(&registered).unwrap()["client_secret"],
        "s-4"
    );
    let public = ClientRegistrationResponse {
        client_secret: None,
        client_secret_expires_at: None,
        ..registered
    };
    let json = serde_json::to_value(&public).unwrap();
    assert!(json.get("client_secret").is_none(), "{json}");
}

#[test]
fn a_registration_ignores_metadata_it_does_not_understand() {
    let r: ClientRegistrationRequest =
        serde_json::from_value(serde_json::json!({
            "redirect_uris": ["https://example.org/cb"],
            "logo_uri": "https://example.org/logo.png",
        }))
        .unwrap();
    assert_eq!(r.token_endpoint_auth_method, "none");
}

#[test]
fn approvals_and_revocations_never_debug_print_a_credential() {
    let a: ApproveRequest = serde_json::from_value(serde_json::json!({
        "client_id": "c",
        "redirect_uri": "https://example.org/cb",
        "operator_email": "op@example.org",
        "operator_password": "pw-1",
        "agent_id": "8a1f3c2e-0000-4000-8000-000000000001",
        "scope": null,
        "state": null,
        "code_challenge": "x",
        "code_challenge_method": "S256",
    }))
    .unwrap();
    assert!(!format!("{a:?}").contains("pw-1"));
    assert_eq!(a.operator_password.expose(), "pw-1");
    let r: RevokeTokenRequest = serde_json::from_value(serde_json::json!({
        "operator_email": "op@example.org",
        "operator_password": "pw-2",
        "token": "tok-3",
    }))
    .unwrap();
    let debug = format!("{r:?}");
    assert!(
        !debug.contains("pw-2") && !debug.contains("tok-3"),
        "{debug}"
    );
    // Unknown fields are refused
    assert!(
        serde_json::from_value::<RevokeTokenRequest>(serde_json::json!({
            "operator_email": "e", "operator_password": "p", "token": "t", "extra": 1,
        }))
        .is_err()
    );
}
