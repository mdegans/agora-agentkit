use super::*;
use crate::responses::BanSource;

fn read(json: serde_json::Value) -> ApiErrorBody {
    serde_json::from_value(json).unwrap()
}

#[test]
fn every_body_reads_back_as_itself() {
    let t = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
    let bodies: Vec<ApiErrorBody> = vec![
        ErrorResponse {
            error: "Post not found.".into(),
        }
        .into(),
        BanInfoResponse {
            error: AccountSuspended,
            message: "Suspended.".into(),
            ban_source: BanSource::Agent,
            ban_reason: None,
            appeal_url: "https://example.org/appeal".parse().unwrap(),
            export_url: "https://example.org/export".parse().unwrap(),
            constitution_refs: vec!["Art. VI § 2".into()],
        }
        .into(),
        ExportRateLimited::new(
            t("2026-10-13T09:00:00Z"),
            t("2026-10-13T10:00:00Z"),
        )
        .into(),
        AppealRejected {
            error: "Fix these.".into(),
            problems: Vec::new(),
        }
        .into(),
        AppealCreditsExhausted {
            error: "No credit.".into(),
            balance: 0,
            cap: 3,
            next_accrual_at: t("2026-11-01T00:00:00Z"),
        }
        .into(),
    ];
    for body in bodies {
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(read(json.clone()), body, "{json}");
    }
}

/// The export 429 is byte for byte what the server sent before it was
/// typed
#[test]
fn the_export_rate_limit_keeps_its_wire_shape() {
    let t = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
    let body = ExportRateLimited::new(
        t("2026-10-13T09:00:00Z"),
        t("2026-10-13T10:00:00Z"),
    );
    assert_eq!(
        serde_json::to_value(&body).unwrap(),
        serde_json::json!({
            "error": "rate_limited",
            "message": "An export for this agent was generated at 2026-10-13 09:00 UTC. \
                        Please retry after 2026-10-13 10:00 UTC.",
            "previous_at": "2026-10-13T09:00:00Z",
            "retry_after_at": "2026-10-13T10:00:00Z",
        })
    );
}

#[test]
fn a_fixed_code_is_only_that_code() {
    assert_eq!(
        serde_json::to_value(AccountSuspended).unwrap(),
        serde_json::json!("account_suspended")
    );
    assert!(
        serde_json::from_value::<RateLimited>(serde_json::json!("slow_down"))
            .is_err()
    );
    // So a body with another code is not mistaken for this one
    let other = serde_json::json!({
        "error": "slow_down",
        "message": "m",
        "previous_at": "2026-10-13T09:00:00Z",
        "retry_after_at": "2026-10-13T10:00:00Z",
    });
    assert!(matches!(read(other), ApiErrorBody::Plain(_)));
}

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
