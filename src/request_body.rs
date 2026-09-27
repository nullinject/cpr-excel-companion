use crate::{Failure, fail};
use axum::http::{HeaderMap, StatusCode};
use serde_json::Value;

/// Limit both decoded bytes and the zstd window; never parse compressed bytes as JSON.
pub const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

pub fn parse(headers: &HeaderMap, body: &[u8], limit: usize) -> Result<Value, Failure> {
    use std::io::Read;

    if body.len() > limit {
        return Err(fail(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body too large",
        ));
    }
    let mut encodings = headers.get_all("content-encoding").iter();
    let encoding = encodings
        .next()
        .map(|v| v.to_str())
        .transpose()
        .map_err(|_| {
            fail(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported content encoding",
            )
        })?
        .unwrap_or("identity")
        .trim();
    // Stacked encodings are not emitted by CPR and must not be silently ignored.
    if encodings.next().is_some() {
        return Err(fail(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported content encoding",
        ));
    }
    let decoded;
    let json = if encoding.eq_ignore_ascii_case("identity") {
        body
    } else if encoding.eq_ignore_ascii_case("zstd") {
        let invalid = |_| fail(StatusCode::BAD_REQUEST, "invalid zstd request body");
        let mut decoder = zstd::stream::read::Decoder::new(body).map_err(invalid)?;
        decoder.window_log_max(25).map_err(invalid)?;
        let mut bytes = Vec::new();
        decoder
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(invalid)?;
        if bytes.len() > limit {
            return Err(fail(
                StatusCode::PAYLOAD_TOO_LARGE,
                "decoded request body too large",
            ));
        }
        decoded = bytes;
        decoded.as_slice()
    } else {
        return Err(fail(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported content encoding",
        ));
    };
    serde_json::from_slice(json).map_err(|_| fail(StatusCode::BAD_REQUEST, "invalid JSON request"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cpr_zstd_request_matches_plain_json() {
        let body = br#"{"model":"gpt-6-sol","input":"hello","stream":true}"#;
        let mut headers = HeaderMap::new();
        let expected = parse(&headers, body, 1024).unwrap();
        headers.insert("content-encoding", "zstd".parse().unwrap());
        let compressed = zstd::stream::encode_all(body.as_slice(), 3).unwrap();
        assert_eq!(parse(&headers, &compressed, 1024).unwrap(), expected);
    }

    #[test]
    fn malformed_and_truncated_zstd_are_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert("content-encoding", "zstd".parse().unwrap());
        let mut truncated = zstd::stream::encode_all(b"{}".as_slice(), 3).unwrap();
        truncated.pop();
        for body in [b"not zstd".as_slice(), truncated.as_slice()] {
            let error = parse(&headers, body, 1024).unwrap_err();
            assert_eq!(error.0, StatusCode::BAD_REQUEST);
            assert_eq!(error.1.0["error"]["message"], "invalid zstd request body");
        }
    }
    #[test]
    fn decoded_size_is_bounded() {
        let mut headers = HeaderMap::new();
        headers.insert("content-encoding", "zstd".parse().unwrap());
        let body = zstd::stream::encode_all(vec![b' '; 4096].as_slice(), 3).unwrap();
        assert_eq!(
            parse(&headers, &body, 1024).unwrap_err().0,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }
    #[test]
    fn unsupported_and_stacked_encodings_are_rejected() {
        for encoding in ["gzip", "zstd, identity", ""] {
            let mut headers = HeaderMap::new();
            headers.insert("content-encoding", encoding.parse().unwrap());
            assert_eq!(
                parse(&headers, b"{}", 1024).unwrap_err().0,
                StatusCode::UNSUPPORTED_MEDIA_TYPE
            );
        }
        let mut headers = HeaderMap::new();
        headers.append("content-encoding", "zstd".parse().unwrap());
        headers.append("content-encoding", "identity".parse().unwrap());
        assert_eq!(
            parse(&headers, b"{}", 1024).unwrap_err().0,
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
    }
    #[test]
    fn identity_and_case_insensitive_encoding_work() {
        let mut headers = HeaderMap::new();
        headers.insert("content-encoding", "identity".parse().unwrap());
        assert_eq!(parse(&headers, b"{}", 2).unwrap(), serde_json::json!({}));
        assert_eq!(
            parse(&headers, b"{} ", 2).unwrap_err().0,
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(
            parse(&headers, b"not json", 1024).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
        headers.insert("content-encoding", "ZsTd".parse().unwrap());
        let body = zstd::stream::encode_all(b"{}".as_slice(), 3).unwrap();
        assert_eq!(parse(&headers, &body, 1024).unwrap(), serde_json::json!({}));
    }
    #[test]
    fn rewritten_upstream_body_does_not_keep_compression_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer test-only".parse().unwrap());
        headers.insert("content-encoding", "zstd".parse().unwrap());
        headers.insert("content-length", "123".parse().unwrap());
        for excel in [false, true] {
            let outgoing = crate::upstream_headers(&headers, excel).unwrap();
            assert!(!outgoing.contains_key("content-encoding"));
            assert!(!outgoing.contains_key("content-length"));
            assert_eq!(outgoing["content-type"], "application/json");
        }
    }

    #[tokio::test]
    async fn http_listener_accepts_compressed_signed_and_unsigned_requests() {
        use crate::{App, Context, excel};
        use std::{
            sync::Arc,
            time::{SystemTime, UNIX_EPOCH},
        };
        let secret = vec![7; 48];
        let absent = std::env::temp_dir().join(format!("excel-test-{}", uuid::Uuid::new_v4()));
        let app = Arc::new(App {
            secret: secret.clone(),
            config_path: absent.join("accounts.json").to_string_lossy().into_owned(),
            control: Arc::new(excel::control::Control::load(absent.join("policy.json")).unwrap()),
            suffix: "-excel".into(),
            allow_unsigned: true,
        });
        let router = axum::Router::new()
            .route(
                "/backend-api/codex/responses",
                axum::routing::post(crate::http),
            )
            .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
            .with_state(app);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/backend-api/codex/responses",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        for mode in [None, Some(false), Some(true)] {
            let mut source = serde_json::json!({"model":"gpt-6-sol","input":"hello"});
            let mut headers = HeaderMap::new();
            if let Some(excel) = mode {
                let ctx = Context {
                    account: "test".into(),
                    scope: "test".into(),
                    request_id: uuid::Uuid::new_v4().to_string(),
                    excel,
                    expires: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        + 60,
                };
                let token = excel::auth::sign(&ctx, &secret).unwrap();
                headers.insert("x-excel-bridge-context", token.parse().unwrap());
                source["client_metadata"] = serde_json::json!({"_cpr_excel_bridge": token});
            }
            let body = serde_json::to_vec(&source).unwrap();
            let plain = client
                .post(&url)
                .headers(headers.clone())
                .body(body.clone())
                .send()
                .await
                .unwrap();
            let status = plain.status();
            let expected = plain.json::<Value>().await.unwrap();
            headers.insert("content-encoding", "zstd".parse().unwrap());
            let encoded = zstd::stream::encode_all(body.as_slice(), 3).unwrap();
            let compressed = client
                .post(&url)
                .headers(headers)
                .body(encoded)
                .send()
                .await
                .unwrap();
            // No upstream is contacted: both requests must reach the missing-map guard.
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(
                expected["error"]["message"],
                "bridge account map unavailable"
            );
            assert_eq!(compressed.status(), status);
            assert_eq!(compressed.json::<Value>().await.unwrap(), expected);
        }
        server.abort();
    }
}
