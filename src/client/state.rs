//! Sealed-state calls (0.147): `/api/state` and key rotation

use url::Url;

use super::{Client, Error, check, sign_hex, signed};
use crate::crypto::SigningKey;
use crate::govlog::Sha256Hex;
use crate::ids::AgentId;
use crate::requests::{
    PostStateEntryPayload, PostStateKeyPayload, RotateKeyPayload,
    StateEntriesQuery,
};
use crate::responses::{
    KeyRotated, StateBlobReceipt, StateEntriesPage, StateEntryCreated,
    StateHeadResponse, StateKeysResponse,
};
use crate::signing::{SignedAction, signed_get};
use crate::state::{KeyRotation, SignedStateEntry, StateKeyRecord};

/// The signer of a sealed-state call
#[derive(Clone, Copy)]
pub struct StateAuth<'a> {
    pub agent_id: AgentId,
    pub key: &'a SigningKey,
}

impl std::fmt::Debug for StateAuth<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "StateAuth {{ agent_id: {}, key: [REDACTED] }}",
            self.agent_id
        )
    }
}

impl Client {
    /// `method url` with the signed-GET headers: the method, host and path
    /// signed. A blob's path names its hash, which binds the body
    fn signed_request(
        &self,
        method: reqwest::Method,
        url: &Url,
        auth: StateAuth<'_>,
    ) -> Result<reqwest::RequestBuilder, Error> {
        let timestamp = chrono::Utc::now().timestamp();
        let path = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_owned(),
        };
        let bytes = signed_get::signed_get_bytes(
            method.as_str(),
            &signed_get::host_of(url),
            &path,
            None,
        )?;
        Ok(self
            .http
            .request(method, url.clone())
            .header(signed_get::AGENT_ID_HEADER, auth.agent_id.to_string())
            .header(signed_get::TIMESTAMP_HEADER, timestamp.to_string())
            .header(
                signed_get::SIGNATURE_HEADER,
                sign_hex(auth.key, &bytes, timestamp),
            ))
    }

    /// Send a signed request, retrying like every other call; it is
    /// signed afresh each attempt
    async fn send_signed(
        &self,
        method: reqwest::Method,
        url: Url,
        auth: StateAuth<'_>,
        body: Option<&[u8]>,
    ) -> Result<reqwest::Response, Error> {
        // Built once to surface a signing error, then per attempt
        let _probe = self.signed_request(method.clone(), &url, auth)?;
        self.send_retrying(&method, &url, || {
            let req = self
                .signed_request(method.clone(), &url, auth)
                .expect("signed once already");
            match body {
                Some(body) => req
                    .header(
                        reqwest::header::CONTENT_TYPE,
                        "application/octet-stream",
                    )
                    .body(body.to_vec()),
                None => req,
            }
        })
        .await
    }

    /// `PUT /api/state/blobs/{sha256}`: a sealed blob, raw. Idempotent
    pub async fn put_state_blob(
        &self,
        auth: StateAuth<'_>,
        sha256: &Sha256Hex,
        sealed: &[u8],
    ) -> Result<StateBlobReceipt, Error> {
        let url =
            self.url_with_segments("api/state/blobs/", &[&sha256.to_hex()])?;
        let resp = self
            .send_signed(reqwest::Method::PUT, url, auth, Some(sealed))
            .await?;
        Ok(check(resp).await?.json().await?)
    }

    /// `GET /api/state/blobs/{sha256}`: one of the signer's blobs
    pub async fn get_state_blob(
        &self,
        auth: StateAuth<'_>,
        sha256: &Sha256Hex,
    ) -> Result<Vec<u8>, Error> {
        let url =
            self.url_with_segments("api/state/blobs/", &[&sha256.to_hex()])?;
        let resp = self
            .send_signed(reqwest::Method::GET, url, auth, None)
            .await?;
        Ok(check(resp).await?.bytes().await?.to_vec())
    }

    /// `POST /api/state/entries`. A head that moved on is
    /// [`Error::Status`] 409 with a [`StateHeadResponse`] body
    /// ([`state_conflict`])
    pub async fn post_state_entry(
        &self,
        auth: StateAuth<'_>,
        entry: &SignedStateEntry,
    ) -> Result<StateEntryCreated, Error> {
        let payload = PostStateEntryPayload {
            entry: entry.clone(),
        };
        let body = signed(
            auth.agent_id,
            &payload,
            &SignedAction::from(&payload),
            auth.key,
            chrono::Utc::now().timestamp(),
        );
        let resp = self.post_json("api/state/entries", &body).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// `GET /api/state/head`
    pub async fn get_state_head(
        &self,
        auth: StateAuth<'_>,
    ) -> Result<StateHeadResponse, Error> {
        let url = self.url("api/state/head")?;
        let resp = self
            .send_signed(reqwest::Method::GET, url, auth, None)
            .await?;
        Ok(check(resp).await?.json().await?)
    }

    /// `GET /api/state/entries`, one page
    pub async fn get_state_entries(
        &self,
        auth: StateAuth<'_>,
        query: &StateEntriesQuery,
    ) -> Result<StateEntriesPage, Error> {
        let mut url = self.url("api/state/entries")?;
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(after) = query.after {
                pairs.append_pair("after", &after.to_string());
            }
            if let Some(limit) = query.limit {
                pairs.append_pair("limit", &limit.to_string());
            }
        }
        if url.query() == Some("") {
            url.set_query(None);
        }
        let resp = self
            .send_signed(reqwest::Method::GET, url, auth, None)
            .await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Every page of [`get_state_entries`](Self::get_state_entries)
    pub async fn get_state_chain(
        &self,
        auth: StateAuth<'_>,
    ) -> Result<Vec<SignedStateEntry>, Error> {
        let mut out = Vec::new();
        let mut after = None;
        loop {
            let page = self
                .get_state_entries(
                    auth,
                    &StateEntriesQuery { after, limit: None },
                )
                .await?;
            out.extend(page.entries.into_iter().map(|e| e.signed));
            match page.next_after {
                Some(next) if Some(next) != after => after = Some(next),
                _ => return Ok(out),
            }
        }
    }

    /// `POST /api/state/keys`
    pub async fn post_state_key(
        &self,
        auth: StateAuth<'_>,
        record: &StateKeyRecord,
    ) -> Result<StateKeysResponse, Error> {
        let payload = PostStateKeyPayload {
            record: record.clone(),
        };
        let body = signed(
            auth.agent_id,
            &payload,
            &SignedAction::from(&payload),
            auth.key,
            chrono::Utc::now().timestamp(),
        );
        let resp = self.post_json("api/state/keys", &body).await?;
        Ok(check(resp).await?.json().await?)
    }

    /// `GET /api/state/keys`. Verify each record before use
    pub async fn get_state_keys(
        &self,
        auth: StateAuth<'_>,
    ) -> Result<StateKeysResponse, Error> {
        let url = self.url("api/state/keys")?;
        let resp = self
            .send_signed(reqwest::Method::GET, url, auth, None)
            .await?;
        Ok(check(resp).await?.json().await?)
    }

    /// `POST /api/identity/rotate_key`, signed by the old key (`auth`)
    pub async fn rotate_key(
        &self,
        auth: StateAuth<'_>,
        rotation: &KeyRotation,
    ) -> Result<KeyRotated, Error> {
        let payload = RotateKeyPayload {
            rotation: rotation.clone(),
        };
        let body = signed(
            auth.agent_id,
            &payload,
            &SignedAction::from(&payload),
            auth.key,
            chrono::Utc::now().timestamp(),
        );
        let resp = self.post_json("api/identity/rotate_key", &body).await?;
        Ok(check(resp).await?.json().await?)
    }
}

/// The head a `409` from [`Client::post_state_entry`] carries
pub fn state_conflict(e: &Error) -> Option<StateHeadResponse> {
    match e {
        Error::Status { status, body, .. }
            if *status == reqwest::StatusCode::CONFLICT =>
        {
            serde_json::from_str(body).ok()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{generate_keypair, verify};
    use httpmock::prelude::*;

    #[tokio::test]
    async fn a_blob_put_is_signed_over_its_method_and_path() {
        let server = MockServer::start_async().await;
        let (key, vk) = generate_keypair();
        let id = AgentId::new();
        let sha = Sha256Hex::from([7; 32]);
        let path = format!("/agora/api/state/blobs/{sha}");
        let mock = server
            .mock_async(|when, then| {
                when.method(PUT)
                    .path(path.clone())
                    .header(signed_get::AGENT_ID_HEADER, id.to_string())
                    .body("sealed");
                then.status(200).json_body(serde_json::json!({
                    "id": uuid::Uuid::nil(),
                    "sha256": sha.to_hex(),
                    "size_bytes": 6,
                }));
            })
            .await;
        let client =
            Client::new(Url::parse(&server.base_url()).unwrap()).unwrap();
        let auth = StateAuth {
            agent_id: id,
            key: &key,
        };
        let receipt =
            client.put_state_blob(auth, &sha, b"sealed").await.unwrap();
        assert_eq!(receipt.size_bytes, 6);
        mock.assert_async().await;

        // What the server checks
        let url = Url::parse(&server.url(&path)).unwrap();
        let req = client
            .signed_request(reqwest::Method::PUT, &url, auth)
            .unwrap()
            .build()
            .unwrap();
        let header = |n: &str| req.headers()[n].to_str().unwrap().to_owned();
        let ts: i64 = header(signed_get::TIMESTAMP_HEADER).parse().unwrap();
        let sig = crate::govlog::SignatureHex::from_str(&header(
            signed_get::SIGNATURE_HEADER,
        ))
        .unwrap();
        let bytes = signed_get::signed_get_bytes(
            "PUT",
            &signed_get::host_of(&url),
            &path,
            None,
        )
        .unwrap();
        assert!(verify(&vk, &bytes, ts, &(&sig).into()));
        let as_get = signed_get::signed_get_bytes(
            "GET",
            &signed_get::host_of(&url),
            &path,
            None,
        )
        .unwrap();
        assert!(!verify(&vk, &as_get, ts, &(&sig).into()));
    }

    #[test]
    fn a_conflict_carries_the_head() {
        let e = Error::Status {
            status: reqwest::StatusCode::CONFLICT,
            body: r#"{"head":null,"stored_bytes":0}"#.into(),
            retry_after: None,
        };
        assert_eq!(state_conflict(&e), Some(StateHeadResponse::default()));
    }

    use std::str::FromStr;
}
