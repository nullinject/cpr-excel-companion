//! 固定上游的桥接服务；只接受官方插件签发的上下文，不保存 OAuth 凭据。
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{
        DefaultBodyLimit, Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use cpr_excel_companion::{self as excel, auth::Context};
use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

type Events = Pin<Box<dyn Stream<Item = Result<Value, &'static str>> + Send>>;
type Failure = (StatusCode, axum::Json<Value>);
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Account {
    proxy: Option<String>,
    direct: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    accounts: BTreeMap<String, Account>,
}
struct App {
    secret: Vec<u8>,
    config_path: String,
    control: Arc<excel::control::Control>,
}
fn fail(status: StatusCode, message: &'static str) -> Failure {
    (
        status,
        axum::Json(json!({"error":{"type":"excel_bridge_error","message":message}})),
    )
}
fn context(headers: &HeaderMap, app: &App) -> Result<Context, Failure> {
    let raw = headers
        .get("x-excel-bridge-context")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| fail(StatusCode::SERVICE_UNAVAILABLE, "bridge context required"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    excel::auth::verify(raw, &app.secret, now)
        .map_err(|_| fail(StatusCode::SERVICE_UNAVAILABLE, "invalid bridge context"))
}
fn client(app: &App, ctx: &Context) -> Result<reqwest::Client, Failure> {
    // 每次请求读取原子替换的代理映射，避免继续使用已撤销的账户或旧代理。
    let data = std::fs::read(&app.config_path).map_err(|_| {
        fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "bridge account map unavailable",
        )
    })?;
    let config: Config = serde_json::from_slice(&data).map_err(|_| {
        fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "invalid bridge account map",
        )
    })?;
    let account = config.accounts.get(&ctx.account).ok_or_else(|| {
        fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "account is not configured for this bridge",
        )
    })?;
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(600));
    match &account.proxy {
        Some(url) if !account.direct => {
            let proxy = reqwest::Proxy::all(url)
                .map_err(|_| fail(StatusCode::SERVICE_UNAVAILABLE, "invalid account proxy"))?;
            builder = builder.proxy(proxy);
        }
        None if account.direct => {}
        _ => {
            return Err(fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "explicit proxy or direct routing is required",
            ));
        }
    }
    builder.build().map_err(|_| {
        fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "unable to construct upstream client",
        )
    })
}
fn upstream_headers(incoming: &HeaderMap, is_excel: bool) -> Result<HeaderMap, Failure> {
    let mut headers = HeaderMap::new();
    if !is_excel {
        // 原链路保留 CPR 生成的业务身份头；桥接鉴权和逐跳传输头不可发往上游。
        for (name, value) in incoming {
            let n = name.as_str();
            if !matches!(
                n,
                "host"
                    | "content-length"
                    | "connection"
                    | "upgrade"
                    | "transfer-encoding"
                    | "accept-encoding"
                    | "x-excel-bridge-context"
            ) && !n.starts_with("sec-websocket-")
            {
                headers.insert(name.clone(), value.clone());
            }
        }
    }
    let auth = incoming
        .get("authorization")
        .filter(|v| v.as_bytes().starts_with(b"Bearer "))
        .ok_or_else(|| {
            fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "CPR OAuth authorization required",
            )
        })?;
    headers.insert("authorization", auth.clone());
    for name in [
        "chatgpt-account-id",
        "x-openai-account-id",
        "x-openai-account-user-id",
        "session-id",
        "conversation-id",
    ] {
        if let Some(value) = incoming.get(name) {
            headers.insert(name, value.clone());
        }
    }
    headers.insert("content-type", "application/json".parse().unwrap());
    headers.insert("accept", "text/event-stream".parse().unwrap());
    if is_excel {
        for (name, value) in [
            ("x-basispoints-auth-mode", "chatgpt"),
            ("origin", "https://bps.openai.com"),
            ("user-agent", "Mozilla/5.0"),
            (
                "x-openai-internal-basispoints-client-agent-profile",
                "excel",
            ),
            ("x-openai-internal-basispoints-client-editor", "excel"),
            ("x-openai-internal-basispoints-client-host", "office"),
            ("x-openai-internal-basispoints-client-platform", "excel"),
            ("x-openai-internal-basispoints-client-platform-class", "PC"),
            (
                "x-openai-internal-basispoints-client-product",
                "basispoints-excel-plugin",
            ),
            ("x-openai-internal-basispoints-client-runtime", "desktop"),
            ("x-openai-internal-basispoints-office-host", "Excel"),
            ("x-openai-internal-basispoints-office-platform", "PC"),
        ] {
            headers.insert(name, value.parse().unwrap());
        }
        if let Some(account) = headers.get("chatgpt-account-id").cloned() {
            headers.insert("x-openai-account-id", account);
        }
    }
    Ok(headers)
}
async fn events(
    app: Arc<App>,
    connection: Context,
    headers: HeaderMap,
    mut source: Value,
) -> Result<Events, Failure> {
    let token = source
        .get_mut("client_metadata")
        .and_then(Value::as_object_mut)
        .and_then(|metadata| metadata.remove("_cpr_excel_bridge"))
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| {
            fail(
                StatusCode::SERVICE_UNAVAILABLE,
                "per-request bridge context required",
            )
        })?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut ctx = excel::auth::verify(&token, &app.secret, now).map_err(|_| {
        fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "invalid per-request bridge context",
        )
    })?;
    if ctx.account != connection.account || ctx.scope != connection.scope {
        return Err(fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "bridge connection scope mismatch",
        ));
    }
    let model = source
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| fail(StatusCode::BAD_REQUEST, "model is required"))?;
    let lease = app
        .control
        .clone()
        .enter(&ctx, model)
        .await
        .map_err(|e| fail(StatusCode::TOO_MANY_REQUESTS, e))?;
    ctx.excel = lease.is_some();
    let client = client(&app, &ctx)?;
    let headers = upstream_headers(&headers, ctx.excel)?;
    let scope = serde_json::to_string(&(&ctx.account, &ctx.scope)).unwrap();
    source
        .as_object_mut()
        .ok_or_else(|| fail(StatusCode::BAD_REQUEST, "request must be an object"))?
        .remove("type");
    let tools;
    let original_input;
    let body;
    if ctx.excel {
        excel::history::restore(&scope, &mut source)
            .map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
        original_input =
            excel::history::input(&source).map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
        excel::attachments::upload_inputs(&client, &headers, &mut source)
            .await
            .map_err(|_| fail(StatusCode::BAD_GATEWAY, "attachment upload failed"))?;
        tools = excel::tool_catalog(&source).map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
        body = excel::prepare(&source, &excel::cache_load(&scope))
            .map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
    } else {
        tools = BTreeMap::new();
        original_input = vec![];
        source["stream"] = json!(true);
        body = source;
    }
    let endpoint = if ctx.excel {
        excel::ENDPOINT
    } else {
        "https://chatgpt.com/backend-api/codex/responses"
    };
    let response = client
        .post(endpoint)
        .headers(headers)
        .json(&body)
        .send()
        .await
        .map_err(|_| fail(StatusCode::BAD_GATEWAY, "upstream connection failed"))?;
    if !response.status().is_success() {
        return Err(fail(response.status(), "upstream rejected request"));
    }
    let mut wire = response.bytes_stream();
    Ok(Box::pin(async_stream::stream! {
        let lease = lease;
        let mut decoder = excel::stream::Decoder::default();
        let mut translator = excel::stream::Translator::new(tools);
        let mut terminal = false;
        while let Some(chunk) = wire.next().await {
            let chunk = match chunk { Ok(c) => c, Err(_) => { yield Err("upstream stream interrupted"); return; } };
            let input = match decoder.push(&chunk) { Ok(v) => v, Err(e) => { yield Err(e); return; } };
            for event in input {
                let output = if ctx.excel { translator.event(event) } else { Ok(vec![event]) };
                let output = match output { Ok(v) => v, Err(e) => { yield Err(e); return; } };
                for event in output {
                    terminal = matches!(event["type"].as_str(), Some("response.completed"|"response.failed"|"response.incomplete"));
                    if terminal && let Some(lease) = lease.as_ref() { lease.terminal(&event); }
                    if terminal && ctx.excel {
                        excel::history::save(&scope, &original_input, &event["response"]);
                        excel::cache_save(&scope, &translator.originals);
                    }
                    yield Ok(event);
                    if terminal { return; }
                }
            }
        }
        if !terminal { yield Err("upstream ended without a terminal event"); }
    }))
}
async fn http(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Failure> {
    let ctx = context(&headers, &app)?;
    let source: Value = serde_json::from_slice(&body)
        .map_err(|_| fail(StatusCode::BAD_REQUEST, "invalid JSON request"))?;
    let mut stream = events(app, ctx, headers, source).await?;
    let bytes = async_stream::stream! {
        while let Some(event) = stream.next().await {
            match event {
                Ok(event) => yield Ok::<Vec<u8>,std::io::Error>(excel::stream::encode(&event)),
                Err(error) => { yield Err(std::io::Error::other(error)); return; }
            }
        }
    };
    Ok((
        [
            ("content-type", "text/event-stream"),
            ("cache-control", "no-cache"),
            ("x-accel-buffering", "no"),
        ],
        Body::from_stream(bytes),
    )
        .into_response())
}
async fn ws(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, Failure> {
    let ctx = context(&headers, &app)?;
    Ok(upgrade
        .max_message_size(32 * 1024 * 1024)
        .on_upgrade(move |socket| websocket(socket, app, ctx, headers)))
}
async fn next_data(socket: &mut WebSocket) -> Option<Result<Message, axum::Error>> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            item => return item,
        }
    }
}
async fn websocket(mut socket: WebSocket, app: Arc<App>, ctx: Context, headers: HeaderMap) {
    while let Some(Ok(message)) = next_data(&mut socket).await {
        let Message::Text(text) = message else {
            if matches!(message, Message::Close(_)) {
                return;
            }
            continue;
        };
        let source: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => {
                let _ = socket.send(Message::Close(None)).await;
                return;
            }
        };
        if source["type"] != "response.create" {
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
        // 每个连接严格串行；发送方断开时立即丢弃上游 future 和响应流。
        let opened = tokio::select! {
            result = events(app.clone(), ctx.clone(), headers.clone(), source) => result,
            _ = next_data(&mut socket) => return,
        };
        match opened {
            Ok(mut stream) => loop {
                let item = tokio::select! { item = stream.next() => item, _ = next_data(&mut socket) => return };
                match item {
                    Some(Ok(event)) => {
                        if socket
                            .send(Message::Text(event.to_string().into()))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    Some(Err(_)) => {
                        let _ = socket.send(Message::Close(None)).await;
                        return;
                    }
                    None => break,
                }
            },
            Err((status, axum::Json(error))) => {
                let event = json!({"type":"error","status":status.as_u16(),"error":error["error"]});
                if socket
                    .send(Message::Text(event.to_string().into()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}

async fn control(
    State(app): State<Arc<App>>,
    Path(operation): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Failure> {
    use sha2::{Digest, Sha256};
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bridge "))
        .ok_or_else(|| fail(StatusCode::UNAUTHORIZED, "control authorization required"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let ctx = excel::auth::verify(token, &app.secret, now)
        .map_err(|_| fail(StatusCode::UNAUTHORIZED, "invalid control authorization"))?;
    let mut hash = Sha256::new();
    hash.update(operation.as_bytes());
    hash.update(b"\n");
    hash.update(&body);
    if ctx.account != "control"
        || ctx.excel
        || ctx.scope != hex::encode(hash.finalize())
        || ctx.expires > now + 60
    {
        return Err(fail(StatusCode::UNAUTHORIZED, "invalid control scope"));
    }
    if body.len() > 65536 {
        return Err(fail(
            StatusCode::PAYLOAD_TOO_LARGE,
            "control payload too large",
        ));
    }
    match operation.as_str() {
        "snapshot" => Ok(axum::Json(app.control.snapshot()).into_response()),
        "policy" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Save {
                policy: excel::admission::Policy,
                expected_version: Option<u64>,
            }
            let value: Save = serde_json::from_slice(&body)
                .map_err(|_| fail(StatusCode::BAD_REQUEST, "invalid policy"))?;
            match app.control.save(value.policy, value.expected_version) {
                Ok(value) => Ok(axum::Json(value).into_response()),
                Err((status, error)) => Ok((
                    StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                    axum::Json(json!({"error":error})),
                )
                    .into_response()),
            }
        }
        _ => Err(fail(StatusCode::NOT_FOUND, "unknown control operation")),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let secret = std::fs::read(std::env::var("EXCEL_BRIDGE_SECRET_FILE")?)?;
    if secret.len() < 32 {
        return Err("bridge secret must contain at least 32 bytes".into());
    }
    let policy_path =
        std::env::var("EXCEL_BRIDGE_POLICY_FILE").unwrap_or_else(|_| "bridge-policy.json".into());
    let app = Arc::new(App {
        control: Arc::new(excel::control::Control::load(policy_path.into())?),
        secret,
        config_path: std::env::var("EXCEL_BRIDGE_ACCOUNT_MAP")?,
    });
    let router = Router::new()
        .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
        .route("/_control/{operation}", post(control))
        .route("/backend-api/codex/responses", post(http).get(ws))
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .with_state(app);
    let listener = tokio::net::TcpListener::bind(
        std::env::var("EXCEL_BRIDGE_LISTEN").unwrap_or_else(|_| "127.0.0.1:8089".into()),
    )
    .await?;
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            #[cfg(unix)]
            {
                if let Ok(mut signal) =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                {
                    tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = signal.recv() => {} }
                } else {
                    let _ = tokio::signal::ctrl_c().await;
                }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
        })
        .await?;
    Ok(())
}
