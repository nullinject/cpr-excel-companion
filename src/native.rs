//! Native responses stay on one upstream WebSocket per downstream connection.
//! No history cache, parameter deletion, cross-connection pooling or retry.
use super::{Account, Events, Failure, fail};
use axum::http::{HeaderMap, StatusCode};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        handshake::{client::generate_key, derive_accept_key},
        protocol::{Role, WebSocketConfig},
    },
};

type Socket = WebSocketStream<reqwest::Upgraded>;
struct Connection {
    socket: Socket,
    route: Account,
    headers: HeaderMap,
}
#[derive(Clone, Default)]
pub struct Session {
    connection: Arc<Mutex<Option<Connection>>>,
}

pub fn http_continuation(source: &Value) -> Result<(), Failure> {
    if source
        .get("previous_response_id")
        .is_some_and(|v| !v.is_null())
    {
        return Err((
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error":{
                "type":"invalid_request_error", "code":"previous_response_not_found",
                "param":"previous_response_id", "message":"Native HTTP has no resumable WebSocket connection; resend full input history"
            }})),
        ));
    }
    Ok(())
}
async fn connect(
    client: &reqwest::Client,
    headers: &HeaderMap,
    endpoint: &str,
) -> Result<Socket, Failure> {
    let key = generate_key();
    let response = client
        .get(endpoint)
        .version(reqwest::Version::HTTP_11)
        .headers(headers.clone())
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", &key)
        .send()
        .await
        .map_err(|_| {
            fail(
                StatusCode::BAD_GATEWAY,
                "native WebSocket connection failed",
            )
        })?;
    if response.status() != StatusCode::SWITCHING_PROTOCOLS {
        eprintln!("native WebSocket handshake status {}", response.status());
        return Err(fail(
            StatusCode::BAD_GATEWAY,
            "native WebSocket handshake rejected",
        ));
    }
    if response
        .headers()
        .get("sec-websocket-accept")
        .and_then(|v| v.to_str().ok())
        != Some(derive_accept_key(key.as_bytes()).as_str())
        || !response
            .headers()
            .get("upgrade")
            .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"))
    {
        return Err(fail(
            StatusCode::BAD_GATEWAY,
            "invalid native WebSocket handshake",
        ));
    }
    let io = response
        .upgrade()
        .await
        .map_err(|_| fail(StatusCode::BAD_GATEWAY, "native WebSocket upgrade failed"))?;
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(32 * 1024 * 1024);
    config.max_frame_size = Some(32 * 1024 * 1024);
    Ok(WebSocketStream::from_raw_socket(io, Role::Client, Some(config)).await)
}
impl Session {
    pub async fn events(
        &self,
        client: reqwest::Client,
        headers: HeaderMap,
        route: Account,
        mut source: Value,
    ) -> Result<Events, Failure> {
        self.open(
            client,
            headers,
            route,
            &mut source,
            "https://chatgpt.com/backend-api/codex/responses",
        )
        .await
    }
    async fn open(
        &self,
        client: reqwest::Client,
        headers: HeaderMap,
        route: Account,
        source: &mut Value,
        endpoint: &str,
    ) -> Result<Events, Failure> {
        let mut slot = self.connection.clone().lock_owned().await;
        let mut connection = match slot.take() {
            Some(c) if c.route == route && c.headers == headers => c,
            _ => Connection {
                socket: connect(&client, &headers, endpoint).await?,
                route,
                headers,
            },
        };
        let object = source
            .as_object_mut()
            .ok_or_else(|| fail(StatusCode::BAD_REQUEST, "request must be an object"))?;
        object.insert("type".into(), json!("response.create"));
        object.remove("stream"); // HTTP-only control, not conversation state.
        connection
            .socket
            .send(Message::Text(source.to_string().into()))
            .await
            .map_err(|_| fail(StatusCode::BAD_GATEWAY, "native WebSocket send failed"))?;
        Ok(Box::pin(async_stream::stream! {
            let started = std::time::Instant::now();
            loop {
                if started.elapsed() >= Duration::from_secs(600) { yield Err("native WebSocket response timed out"); return; }
                let item = match tokio::time::timeout(Duration::from_secs(15), connection.socket.next()).await {
                    Ok(item) => item,
                    Err(_) => { yield Ok(json!({"type":"bridge.comment","text":"keepalive"})); continue; }
                };
                match item {
                    Some(Ok(Message::Text(text))) => {
                        let event: Value = match serde_json::from_str(&text) {
                            Ok(event) => event,
                            Err(_) => { yield Err("invalid native WebSocket event"); return; }
                        };
                        if super::excel::stream::is_terminal(&event) || event["type"] == "error" {
                            *slot = Some(connection);
                            yield Ok(event);
                            return;
                        }
                        yield Ok(event);
                    }
                    Some(Ok(Message::Ping(_))) => {
                        if connection.socket.flush().await.is_err() { yield Err("native WebSocket pong failed"); return; }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    _ => { yield Err("native WebSocket closed before terminal event"); return; }
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn native_two_turns_keep_previous_id_on_the_same_socket() {
        use axum::{Router, extract::WebSocketUpgrade, response::IntoResponse, routing::get};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/", get(|upgrade: WebSocketUpgrade| async move {
                upgrade.on_upgrade(|mut ws| async move {
                    for turn in 0..2 {
                        let axum::extract::ws::Message::Text(text) = ws.recv().await.unwrap().unwrap() else { panic!("text required") };
                        let source: Value = serde_json::from_str(&text).unwrap();
                        assert_eq!(source["type"], "response.create");
                        assert!(source.get("stream").is_none());
                        if turn == 1 { assert_eq!(source["previous_response_id"], "resp_native_first"); assert_eq!(source["input"], "repeat marker"); }
                        let event = json!({"type":"response.completed","response":{"id":if turn==0 {"resp_native_first"} else {"resp_native_second"},"status":"completed","output":[]}});
                        ws.send(axum::extract::ws::Message::Text(event.to_string().into())).await.unwrap();
                    }
                }).into_response()
            }))).await.unwrap();
        });
        let session = Session::default();
        let route = Account {
            proxy: None,
            direct: true,
            upstream_account_id: None,
        };
        let client = super::super::client_for_route(&route).unwrap();
        for (input, previous, expected) in [
            ("remember marker", None, "resp_native_first"),
            (
                "repeat marker",
                Some("resp_native_first"),
                "resp_native_second",
            ),
        ] {
            let mut source = json!({"input":input,"stream":true});
            if let Some(previous) = previous {
                source["previous_response_id"] = json!(previous);
            }
            let events = session
                .open(
                    client.clone(),
                    HeaderMap::new(),
                    route.clone(),
                    &mut source,
                    &endpoint,
                )
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await;
            assert_eq!(events[0].as_ref().unwrap()["response"]["id"], expected);
        }
        let source = json!({"previous_response_id":"resp_other_connection","input":"keep this"});
        let original = source.clone();
        let (_, error) = http_continuation(&source).unwrap_err();
        assert_eq!(error["error"]["code"], "previous_response_not_found");
        assert_eq!(source, original);
        server.abort();
    }
}
