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
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

mod request_body;
mod native;

type Events = Pin<Box<dyn Stream<Item = Result<Value, &'static str>> + Send>>;
type Failure = (StatusCode, axum::Json<Value>);
#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Account {
    proxy: Option<String>,
    direct: bool,
    #[serde(default)]
    upstream_account_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    accounts: BTreeMap<String, Account>,
    /// 同步脚本写入的元数据；桥接只读 accounts。
    #[serde(default, rename = "version")]
    _version: Option<String>,
}
struct App {
    secret: Vec<u8>,
    config_path: String,
    control: Arc<excel::control::Control>,
    suffix: String,
    allow_unsigned: bool,
    clients: Mutex<BTreeMap<String, (Account, reqwest::Client)>>,
}
fn fail(status: StatusCode, message: &'static str) -> Failure {
    if matches!(message, excel::history::MISSING | excel::history::CACHE_LIMIT) {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error":{
                "type":"invalid_request_error",
                "code":"previous_response_not_found",
                "param":"previous_response_id",
                "message":message
            }})),
        );
    }
    (
        status,
        axum::Json(json!({"error":{"type":"excel_bridge_error","message":message}})),
    )
}
#[cfg(test)]
mod continuation_error_tests {
    use super::*;
    #[test]
    fn cache_limit_requests_replay_without_losing_input() {
        excel::history::save("limit-test-scope", &[json!({"role":"user","content":"prior"})],
            &json!({"id":"resp_limit_test","status":"completed","output":[]}));
        let mut source = json!({"previous_response_id":"resp_limit_test","input":"x".repeat(2 * 1024 * 1024)});
        let original = source.clone();
        let message = excel::history::restore("limit-test-scope", &mut source).unwrap_err();
        assert_eq!(message, excel::history::CACHE_LIMIT);
        let (status, body) = fail(StatusCode::BAD_REQUEST, message);
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "previous_response_not_found");
        assert_eq!(source, original);
        source.as_object_mut().unwrap().remove("previous_response_id");
        excel::history::restore("limit-test-scope", &mut source).unwrap();
    }
    #[test]
    fn missing_continuation_has_standard_replay_code() {
        let mut source = json!({"previous_response_id":"resp_missing_test","input":"next"});
        let original = source.clone();
        let error = excel::history::restore("missing-test-scope", &mut source).unwrap_err();
        let (status, body) = fail(StatusCode::BAD_REQUEST, error);
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "previous_response_not_found");
        assert_eq!(body["error"]["param"], "previous_response_id");
        assert_eq!(source, original);
    }
}
/// 连接级身份：有签名为 Some(ctx)；无签名为 None（仅原生透传，见 App::allow_unsigned）。
#[derive(Clone)]
enum Identity {
    Signed(Box<Context>),
    Unsigned,
}
fn context(headers: &HeaderMap, app: &App) -> Result<Identity, Failure> {
    let Some(raw) = headers.get("x-excel-bridge-context").and_then(|v| v.to_str().ok()) else {
        // 绑定未命中的 key（宿主不调用插件）走这里：只允许原生透传。
        if app.allow_unsigned {
            return Ok(Identity::Unsigned);
        }
        return Err(fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "bridge context required",
        ));
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    excel::auth::verify(raw, &app.secret, now)
        .map(|ctx| Identity::Signed(Box::new(ctx)))
        .map_err(|_| fail(StatusCode::SERVICE_UNAVAILABLE, "invalid bridge context"))
}
fn client(app: &App, ctx: &Context) -> Result<reqwest::Client, Failure> {
    // 每次请求读取原子替换的代理映射，避免继续使用已撤销的账户或旧代理。
    let config = read_config(app)?;
    let route = config.accounts.get(&ctx.account).ok_or_else(|| {
        fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "account is not configured for this bridge",
        )
    })?;
    cached_client(app, &ctx.account, route)
}
fn unsigned_client(app: &App, account_id: Option<&str>) -> Result<reqwest::Client, Failure> {
    let config = read_config(app)?;
    let id = account_id.unwrap_or_default();
    let route = unsigned_route(&config, Some(id))?;
    cached_client(app, id, route)
}
fn unsigned_route<'a>(config: &'a Config, account_id: Option<&str>) -> Result<&'a Account, Failure> {
    let id = account_id.filter(|id| !id.is_empty()).ok_or_else(|| {
        fail(StatusCode::SERVICE_UNAVAILABLE, "upstream account identity required")
    })?;
    if let Some(route) = config.accounts.get(id) {
        return Ok(route);
    }
    // CPR's account id and ChatGPT's account id are different namespaces.
    // Resolve the latter explicitly; never guess another account's egress.
    let mut matches = config.accounts.values().filter(|route| {
        route.upstream_account_id.as_deref() == Some(id)
    });
    let route = matches.next().ok_or_else(|| {
        fail(StatusCode::SERVICE_UNAVAILABLE, "upstream account is not mapped")
    })?;
    if matches.next().is_some() {
        return Err(fail(StatusCode::SERVICE_UNAVAILABLE, "upstream account route is ambiguous"));
    }
    Ok(route)
}
fn read_config(app: &App) -> Result<Config, Failure> {
    let data = std::fs::read(&app.config_path).map_err(|_| {
        fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "bridge account map unavailable",
        )
    })?;
    serde_json::from_slice(&data).map_err(|_| {
        fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "invalid bridge account map",
        )
    })
}
fn cached_client(app: &App, id: &str, route: &Account) -> Result<reqwest::Client, Failure> {
    let mut clients = app
        .clients
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((previous, client)) = clients.get(id)
        && previous == route
    {
        return Ok(client.clone());
    }
    let client = client_for_route(route)?;
    if clients.len() >= 256 {
        clients.clear();
    }
    clients.insert(id.to_owned(), (route.clone(), client.clone()));
    Ok(client)
}
// Full upstream errors stay in a private, rotating server log, not client responses.
fn upstream_error_log(app: &App) -> excel::error_log::ErrorLog {
    let parent = std::path::Path::new(&app.config_path).parent().unwrap_or_else(|| std::path::Path::new("."));
    excel::error_log::ErrorLog::new(parent.join("error-logs/upstream-errors.jsonl"))
}
fn error_log_secrets(app: &App, headers: &HeaderMap, context_token: &str) -> Vec<String> {
    let mut values = vec![context_token.to_owned(), String::from_utf8_lossy(&app.secret).into_owned()];
    for name in ["authorization", "proxy-authorization", "cookie", "x-api-key", "x-excel-bridge-context"] {
        if let Some(value) = headers.get(name).and_then(|v| v.to_str().ok()) {
            values.push(value.to_owned());
            if let Some((scheme, token)) = value.split_once(' ') {
                if scheme.eq_ignore_ascii_case("bearer") || scheme.eq_ignore_ascii_case("basic") { values.push(token.to_owned()); }
            }
        }
    }
    values
}

// Opt-in, short-lived forensic capture. Never capture headers, credentials or full requests.
// An operator supplies a key hash + expiry; at most 8 failed tool events are saved privately.
fn capture_tool_failure(app: &App, ctx: &Context, event: &Value) {
    let Some(parent) = std::path::Path::new(&app.config_path).parent() else {
        return;
    };
    let Ok(bytes) = std::fs::read(parent.join("diagnostics.json")) else {
        return;
    };
    let Ok(config) = serde_json::from_slice::<Value>(&bytes) else {
        return;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let expiry = config["expires"].as_u64().unwrap_or(0);
    if expiry <= now
        || expiry > now + 1800
        || ctx.key.as_deref() != config["key_hash"].as_str()
        || ctx.key.is_none()
    {
        return;
    }
    if event.pointer("/item/type").and_then(Value::as_str) != Some("function_call") {
        return;
    }
    let data = json!({"request_id":ctx.request_id,"event":event}).to_string();
    if data.len() > 128 * 1024 {
        return;
    }
    use std::io::Write;
    for index in 0..8 {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        if let Ok(mut file) = options.open(parent.join(format!("tool-diagnostic-{index}.json"))) {
            if file.write_all(data.as_bytes()).is_err() {
                eprintln!("tool diagnostic write failed");
            }
            break;
        }
    }
}
fn client_for_route(account: &Account) -> Result<reqwest::Client, Failure> {
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(3)
        .tcp_keepalive(Duration::from_secs(30))
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
        // 反代/CDN 注入的头（forwarded 系、cf-*）会让上游风控，一并剥离。
        for (name, value) in incoming {
            let n = name.as_str();
            if !matches!(
                n,
                "host"
                    | "content-length"
                    | "content-encoding"
                    | "connection"
                    | "upgrade"
                    | "transfer-encoding"
                    | "accept-encoding"
                    | "x-excel-bridge-context"
                    | "x-forwarded-for"
                    | "x-forwarded-host"
                    | "x-forwarded-proto"
                    | "x-real-ip"
                    | "cdn-loop"
                    | "forwarded"
            ) && !n.starts_with("sec-websocket-")
                && !n.starts_with("cf-")
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
        // 头集合对齐 excel-codex-bridge 的加载项指纹；UA 可按部署覆盖。
        let ua = std::env::var("EXCEL_BRIDGE_UPSTREAM_UA").unwrap_or_else(|_| "Mozilla/5.0".into());
        for (name, value) in [
            ("x-basispoints-auth-mode", "chatgpt"),
            ("origin", "https://bps.openai.com"),
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
            ("x-stainless-arch", "unknown"),
            ("x-stainless-lang", "js"),
            ("x-stainless-os", "Unknown"),
            ("x-stainless-package-version", "6.31.0"),
            ("x-stainless-retry-count", "0"),
            ("x-stainless-runtime", "browser:chrome"),
        ] {
            headers.insert(name, value.parse().unwrap());
        }
        headers.insert("user-agent", ua.parse().unwrap());
        if let Some(account) = headers.get("chatgpt-account-id").cloned() {
            headers.insert("x-openai-account-id", account);
        }
    }
    Ok(headers)
}
// Once streaming starts, report a failed terminal instead of an unexplained EOF.
fn terminal_errors(mut stream: Events) -> Events {
    Box::pin(async_stream::stream! {
        let mut response_id = "resp_excel_failed".to_owned();
        let mut sequence = 0_u64;
        let error = loop {
            match stream.next().await {
                Some(Ok(event)) => {
                    if let Some(id) = event.pointer("/response/id").and_then(Value::as_str) {
                        response_id = id.to_owned();
                    }
                    if let Some(number) = event["sequence_number"].as_u64() {
                        sequence = number.saturating_add(1);
                    }
                    let terminal = excel::stream::is_terminal(&event);
                    yield Ok(event);
                    // A late socket close after a valid terminal is not another failure.
                    if terminal { return; }
                }
                Some(Err(error)) => break error,
                None => break "upstream ended without a terminal event",
            }
        };
        // Only static bridge diagnostics, never arbitrary upstream error payloads.
        yield Ok(json!({"type":"response.failed","sequence_number":sequence,
            "response":{"id":response_id,"object":"response","status":"failed",
            "output":[],"error":{"type":"server_error","code":"excel_bridge_stream_error","message":error}}}));
    })
}
async fn events(
    app: Arc<App>,
    identity: Identity,
    headers: HeaderMap,
    source: Value,
    native: Option<native::Session>,
) -> Result<Events, Failure> {
    open_events(app, identity, headers, source, native)
        .await
        .map(terminal_errors)
}

async fn open_events(
    app: Arc<App>,
    identity: Identity,
    headers: HeaderMap,
    source: Value,
    native: Option<native::Session>,
) -> Result<Events, Failure> {
    open_events_at(app, identity, headers, source, native, excel::ENDPOINT).await
}
// The endpoint is internal, never request/config controlled; tests use a loopback fixture.
async fn open_events_at(
    app: Arc<App>,
    identity: Identity,
    headers: HeaderMap,
    mut source: Value,
    native: Option<native::Session>,
    excel_endpoint: &'static str,
) -> Result<Events, Failure> {
    let started = Instant::now();
    // 未签名连接：绑定未命中的 key，仅原生透传；-excel 后缀在此还原避免上游拒绝。
    let connection = match identity {
        Identity::Unsigned => {
            let model = source
                .get("model")
                .and_then(Value::as_str)
                .and_then(|m| m.strip_suffix(&app.suffix))
                .filter(|m| !m.is_empty())
                .map(str::to_owned);
            return unsigned_events(app, headers, source, model, native).await;
        }
        Identity::Signed(ctx) => *ctx,
    };
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
        .ok_or_else(|| fail(StatusCode::BAD_REQUEST, "model is required"))?
        .to_owned();
    let lease = app.control.enter(&ctx, &model, &app.suffix).await;
    if lease.rejected {
        return Err(fail(
            StatusCode::TOO_MANY_REQUESTS,
            "Excel concurrency or queue limit reached",
        ));
    }
    ctx.excel = lease.excel;
    // 早期失败的桥接侧原因也写入记录，便于侧边栏排障。
    let note = |message: String| {
        lease.fail(&message);
        eprintln!("bridge request {} failed: {message}", ctx.request_id);
    };
    let scope = serde_json::to_string(&(&ctx.account, &ctx.scope)).unwrap();
    if ctx.excel && source.get("generate") == Some(&Value::Bool(false)) {
        let warmup = excel::history::prewarm(&scope, &source).map_err(|error| {
            lease.fail(error);
            fail(StatusCode::BAD_REQUEST, error)
        })?;
        if let Some(terminal) = warmup.last() {
            lease.terminal(terminal);
        }
        return Ok(Box::pin(futures_util::stream::iter(
            warmup.into_iter().map(Ok),
        )));
    }
    let client = match client(&app, &ctx) {
        Ok(client) => client,
        Err((status, error)) => {
            let message = error["error"]["message"]
                .as_str()
                .unwrap_or("client build failed")
                .to_owned();
            note(message);
            return Err((status, error));
        }
    };
    let headers = match upstream_headers(&headers, ctx.excel) {
        Ok(headers) => headers,
        Err((status, error)) => {
            note("upstream headers rejected".into());
            return Err((status, error));
        }
    };
    source
        .as_object_mut()
        .ok_or_else(|| fail(StatusCode::BAD_REQUEST, "request must be an object"))?
        .remove("type");
    let tools;
    let original_input;
    let body;
    if ctx.excel {
        if let Err(e) = excel::history::restore(&scope, &mut source) {
            note(format!("history restore: {e}"));
            return Err(fail(StatusCode::BAD_REQUEST, e));
        }
        original_input =
            excel::history::input(&source).map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
        if let Err(_e) = excel::attachments::upload_inputs(&client, &headers, &mut source).await {
            note("attachment upload failed".into());
            return Err(fail(StatusCode::BAD_GATEWAY, "attachment upload failed"));
        }
        tools = excel::tool_catalog(&source);
        if app.control.normalize_user_prompt() {
            let applied = excel::prompt_normalization::apply(&mut source);
            eprintln!("bridge prompt_normalization {} applied={} original_input_preserved=true", ctx.request_id, applied);
        }
        body = match excel::prepare(&source, &excel::cache_load(&scope)) {
            Ok(body) => body,
            Err(e) => {
                note(format!("prepare: {e}"));
                return Err(fail(StatusCode::BAD_REQUEST, e));
            }
        };
    } else {
        tools = BTreeMap::new();
        original_input = vec![];
        if let Some(object) = source.as_object_mut() {
            // 通道覆盖为原生时收到的可能带后缀；上游不认识，统一还原。
            if let Some(Value::String(m)) = object.get("model") && let Some(base) =
                m.strip_suffix(&app.suffix).filter(|b| !b.is_empty())
            {
                object.insert("model".into(), json!(base));
            }
            object.insert("stream".into(), json!(true));
        }
        if let Some(session) = native {
            let config = read_config(&app)?;
            let route = config.accounts.get(&ctx.account).ok_or_else(|| fail(StatusCode::SERVICE_UNAVAILABLE, "account is not configured for this bridge"))?.clone();
            let mut upstream = session.events(client, headers, route, source).await.map_err(|error| { lease.fail("native WebSocket open failed"); error })?;
            return Ok(Box::pin(async_stream::stream! {
                while let Some(event) = upstream.next().await {
                    match &event {
                        Ok(event) if excel::stream::is_terminal(event) || event["type"] == "error" => lease.terminal(event),
                        Err(error) => lease.fail(error),
                        _ => {}
                    }
                    yield event;
                }
            }));
        }
        native::http_continuation(&source)?;
        body = source;
    }
    let endpoint = if ctx.excel {
        excel_endpoint
    } else {
        "https://chatgpt.com/backend-api/codex/responses"
    };
    let error_secrets = error_log_secrets(&app, &headers, &token);
    let upstream_started = Instant::now();
    let response = match client
        .post(endpoint)
        .headers(headers.clone())
        .json(&body)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            note(format!("upstream connection failed: {error}"));
            return Err(fail(StatusCode::BAD_GATEWAY, "upstream connection failed"));
        }
    };
    if !response.status().is_success() {
        let status = response.status();
        // 上游拒绝原因对排障关键；截断后随错误体返回给宿主重试链路。
        let detail = response.text().await.unwrap_or_default();
        let secrets: Vec<&str> = error_secrets.iter().map(String::as_str).collect();
        if let Err(error) = upstream_error_log(&app).record_http(&ctx.request_id, status.as_u16(), &detail, &secrets) {
            eprintln!("bridge error_record_failed {} kind={:?}", ctx.request_id, error.kind());
        }
        // Preserve the bounded client response while excluding credentials.
        let parsed = serde_json::from_str::<Value>(&detail).unwrap_or_else(|_| json!(detail));
        let clean = excel::error_log::sanitize(&parsed, &secrets);
        let detail = clean.as_str().map(str::to_owned).unwrap_or_else(|| clean.to_string());
        eprintln!("bridge request {} upstream status {status}", ctx.request_id);
        let mut message = format!("upstream status {status}");
        let detail = detail.trim();
        if !detail.is_empty() {
            let cut = detail
                .char_indices()
                .nth(512)
                .map_or(detail.len(), |(i, _)| i);
            message = format!("upstream rejected request: {}", &detail[..cut]);
        }
        return Err((
            status,
            axum::Json(json!({"error":{"type":"excel_bridge_error","message":message}})),
        ));
    }
    eprintln!(
        "bridge timing {} prepared_ms={} upstream_headers_ms={} status={}",
        ctx.request_id,
        upstream_started.duration_since(started).as_millis(),
        upstream_started.elapsed().as_millis(),
        response.status().as_u16()
    );
    let retry_enabled = ctx.excel && app.control.retry_policy_errors();
    let upstream_http_status = response.status().as_u16();
    let mut wire = response.bytes_stream();
    Ok(Box::pin(async_stream::stream! {
        let lease = lease;
        let mut decoder = excel::stream::Decoder::default();
        let mut translator = excel::stream::Translator::new(tools.clone());
        let mut retry = excel::stream::RejectionRetry::new(retry_enabled);
        let mut terminal = false;
        let mut first_event = true;
        let mut last_event = String::new();
        // 上游静默期插 SSE 注释保活：反代（Cloudflare ~100s 空闲超时）不会掐断长思考。
        'upstream: loop {
            let chunk = match tokio::time::timeout(Duration::from_secs(15), wire.next()).await {
                Ok(Some(Ok(c))) => c,
                Ok(Some(Err(_))) => { lease.fail("upstream stream interrupted"); yield Err("upstream stream interrupted"); return; }
                Ok(None) => break,
                Err(_elapsed) => {
                    yield Ok(json!({"type":"bridge.comment","text":"keepalive"}));
                    continue;
                }
            };
            let input = match decoder.push(&chunk) { Ok(v) => v, Err(e) => { lease.fail(e); eprintln!("bridge stream {} failed: {e}", ctx.request_id); yield Err(e); return; } };
            for event in input {
                last_event = event["type"].as_str().unwrap_or("unknown").to_owned();
                if first_event { eprintln!("bridge timing {} first_event_ms={}", ctx.request_id, started.elapsed().as_millis()); first_event = false; }
                if matches!(last_event.as_str(), "error" | "response.failed" | "response.incomplete") {
                    let secrets: Vec<&str> = error_secrets.iter().map(String::as_str).collect();
                    if let Err(error) = upstream_error_log(&app).record_event(&ctx.request_id, upstream_http_status, &event, &secrets) {
                        eprintln!("bridge error_record_failed {} kind={:?}", ctx.request_id, error.kind());
                    }
                    let (code, _, _) = excel::stream::failure_details(&event);
                    eprintln!("bridge upstream_terminal {} event={} code={} reason={} elapsed_ms={} error_fields={}",
                        ctx.request_id, last_event, code, excel::stream::incomplete_reason(&event), started.elapsed().as_millis(), excel::stream::failure_diagnostics(&event));
                }
                let events = match retry.event(event) {
                    excel::stream::RetryDecision::Hold => continue,
                    excel::stream::RetryDecision::Forward(events) => events,
                    excel::stream::RetryDecision::Retry(original) => {
                        let code = excel::stream::failure_details(original.last().unwrap()).0;
                        let retry_number = retry.retries();
                        eprintln!("bridge policy_retry {} code={} retry={}/{} same_request=true before_output=true", ctx.request_id, code, retry_number, excel::stream::MAX_POLICY_RETRIES);
                        tokio::time::sleep(Duration::from_millis(500 * (1_u64 << (retry_number - 1)))).await;
                        // Keep the same client/account/headers/body, and respect account revocation.
                        let authorized = read_config(&app).is_ok_and(|config| config.accounts.contains_key(&ctx.account));
                        if authorized {
                            match client.post(endpoint).headers(headers.clone()).json(&body).send().await {
                                Ok(response) if response.status().is_success() => {
                                    wire = response.bytes_stream();
                                    decoder = excel::stream::Decoder::default();
                                    translator = excel::stream::Translator::new(tools.clone());
                                    continue 'upstream;
                                }
                                _ => eprintln!("bridge policy_retry {} reopen_failed=true original_rejection_retained=true", ctx.request_id),
                            }
                        } else {
                            eprintln!("bridge policy_retry {} account_unavailable=true original_rejection_retained=true", ctx.request_id);
                        }
                        original
                    }
                };
                for event in events {
                let output = if ctx.excel { translator.event(event.clone()) } else { Ok(vec![event.clone()]) };
                if output.is_err() { capture_tool_failure(&app, &ctx, &event); eprintln!("bridge timing {} translation_failed_ms={} last_event={}", ctx.request_id, started.elapsed().as_millis(), last_event); }
                let output = match output { Ok(v) => v, Err(e) => { lease.fail(e); eprintln!("bridge stream {} failed: {e}", ctx.request_id); yield Err(e); return; } };
                for mut event in output {
                    // A client can quote this ID without exposing any upstream error text.
                    if ctx.excel && event["type"] == "response.failed" {
                        if let Some(message) = event.pointer("/response/error/message").and_then(Value::as_str) {
                            event["response"]["error"]["message"] = json!(format!("{message} [request_id={}]", ctx.request_id));
                            event["response"]["error"]["request_id"] = json!(ctx.request_id);
                        }
                    }
                    terminal = excel::stream::is_terminal(&event);
                    if terminal { lease.terminal(&event); eprintln!("bridge timing {} terminal_ms={} event={}", ctx.request_id, started.elapsed().as_millis(), event["type"]); }
                    if terminal && ctx.excel {
                        excel::history::save(&scope, &original_input, &event["response"]);
                        excel::cache_save(&scope, &translator.originals);
                    }
                    yield Ok(event);
                    if terminal { return; }
                }
                }
            }
        }
        if let Err(e) = decoder.finish() { lease.fail(e); yield Err(e); return; }
        if !terminal { lease.fail("upstream ended without a terminal event"); eprintln!("bridge EOF {} elapsed_ms={} last_event={}", ctx.request_id, started.elapsed().as_millis(), last_event); yield Err("upstream ended without a terminal event"); }
    }))
}
/// 绑定未命中 key 的原生透传：无签名、无 Excel 转换、不占用 Excel 并发预算。
async fn unsigned_events(
    app: Arc<App>,
    headers: HeaderMap,
    mut source: Value,
    stripped_model: Option<String>,
    native: Option<native::Session>,
) -> Result<Events, Failure> {
    let model = match stripped_model {
        Some(model) => model,
        None => source
            .get("model")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
            .ok_or_else(|| fail(StatusCode::BAD_REQUEST, "model is required"))?
            .to_owned(),
    };
    if let Some(object) = source.as_object_mut() {
        object.insert("model".into(), json!(model));
        object.remove("type");
        object.insert("stream".into(), json!(true));
    }
    let request_id = uuid::Uuid::new_v4().to_string();
    let account_id = headers
        .get("chatgpt-account-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let lease = app
        .control
        .enter_unsigned(&request_id, &model, account_id.as_deref())
        .map_err(|e| fail(StatusCode::SERVICE_UNAVAILABLE, e))?;
    let client = match unsigned_client(&app, account_id.as_deref()) {
        Ok(client) => client,
        Err((status, error)) => {
            lease.fail("unsigned client build failed");
            return Err((status, error));
        }
    };
    let upstream = match upstream_headers(&headers, false) {
        Ok(upstream) => upstream,
        Err((status, error)) => {
            lease.fail("upstream headers rejected");
            return Err((status, error));
        }
    };
    if let Some(session) = native {
        let config = read_config(&app)?;
        let route = unsigned_route(&config, account_id.as_deref())?.clone();
        let mut events = session.events(client, upstream, route, source).await.map_err(|error| { lease.fail("native WebSocket open failed"); error })?;
        app.control.running(&request_id, 0);
        return Ok(Box::pin(async_stream::stream! {
            while let Some(event) = events.next().await {
                match &event {
                    Ok(event) if excel::stream::is_terminal(event) || event["type"] == "error" => app.control.finish_unsigned(&request_id, event),
                    Err(error) => lease.fail(error),
                    _ => {}
                }
                yield event;
            }
        }));
    }
    native::http_continuation(&source)?;
    let response = match client
        .post("https://chatgpt.com/backend-api/codex/responses")
        .headers(upstream)
        .json(&source)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            lease.fail(&format!("upstream connection failed: {error}"));
            return Err(fail(StatusCode::BAD_GATEWAY, "upstream connection failed"));
        }
    };
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        let trimmed = detail.trim();
        let cut = trimmed.char_indices().nth(512).map_or(trimmed.len(), |(i, _)| i);
        lease.fail(&format!("upstream status {status}: {}", &trimmed[..cut]));
        return Err((
            status,
            axum::Json(json!({"error":{"type":"excel_bridge_error",
                "message":format!("upstream status {status}")}})),
        ));
    }
    app.control.running(&request_id, 0);
    let mut wire = response.bytes_stream();
    Ok(Box::pin(async_stream::stream! {
        let mut decoder = excel::stream::Decoder::default();
        let mut terminal = false;
        loop {
            let chunk = match tokio::time::timeout(Duration::from_secs(15), wire.next()).await {
                Ok(Some(Ok(c))) => c,
                Ok(Some(Err(_))) => { lease.fail("upstream stream interrupted"); yield Err("upstream stream interrupted"); return; }
                Ok(None) => break,
                Err(_elapsed) => {
                    yield Ok(json!({"type":"bridge.comment","text":"keepalive"}));
                    continue;
                }
            };
            let input = match decoder.push(&chunk) { Ok(v) => v, Err(e) => { lease.fail(e); yield Err(e); return; } };
            for event in input {
                terminal = excel::stream::is_terminal(&event);
                if terminal {
                    app.control.finish_unsigned(&request_id, &event);
                }
                yield Ok(event);
                if terminal { return; }
            }
        }
        if let Err(e) = decoder.finish() { lease.fail(e); yield Err(e); return; }
        if !terminal { lease.fail("upstream ended without a terminal event"); yield Err("upstream ended without a terminal event"); }
    }))
}
/// Model discovery uses the same authenticated account and configured egress.
/// Only this fixed upstream resource is forwarded; never an arbitrary URL.
async fn models(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> Result<Response, Failure> {
    let identity = context(&headers, &app)?;
    let client = match identity {
        Identity::Signed(ctx) => client(&app, &ctx)?,
        Identity::Unsigned => unsigned_client(&app, headers.get("chatgpt-account-id").and_then(|value| value.to_str().ok()))?,
    };
    let mut upstream = upstream_headers(&headers, false)?;
    upstream.insert("accept", "application/json".parse().unwrap());
    let mut url = reqwest::Url::parse("https://chatgpt.com/backend-api/codex/models").unwrap();
    url.set_query(query.as_deref());
    let response = client.get(url).headers(upstream).send().await
        .map_err(|_| fail(StatusCode::BAD_GATEWAY, "native model directory connection failed"))?;
    let status = response.status();
    let mut output = HeaderMap::new();
    for name in ["content-type", "etag", "cache-control"] {
        if let Some(value) = response.headers().get(name) { output.insert(name, value.clone()); }
    }
    Ok((status, output, Body::from_stream(response.bytes_stream())).into_response())
}

async fn http(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Failure> {
    let identity = context(&headers, &app)?;
    let decode_headers = headers.clone();
    let source = tokio::task::spawn_blocking(move || {
        request_body::parse(&decode_headers, &body, request_body::MAX_BODY_BYTES)
    })
    .await
    .map_err(|_| fail(StatusCode::INTERNAL_SERVER_ERROR, "request decoding failed"))??;
    let mut stream = events(app, identity, headers, source, None).await?;
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
    let identity = context(&headers, &app)?;
    Ok(upgrade
        .max_message_size(32 * 1024 * 1024)
        .on_upgrade(move |socket| websocket(socket, app, identity, headers)))
}
async fn next_data(socket: &mut WebSocket) -> Option<Result<Message, axum::Error>> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            item => return item,
        }
    }
}
async fn websocket(mut socket: WebSocket, app: Arc<App>, identity: Identity, headers: HeaderMap) {
    let native = native::Session::default();
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
            result = events(app.clone(), identity.clone(), headers.clone(), source, Some(native.clone())) => result,
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
        "observe" => {
            let observation: excel::observe::ObserveEvent = serde_json::from_slice(&body)
                .map_err(|_| fail(StatusCode::BAD_REQUEST, "invalid observation"))?;
            app.control.observe(&observation, &app.suffix);
            Ok((StatusCode::NO_CONTENT, axum::Json(json!({}))).into_response())
        }
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
        clients: Mutex::new(BTreeMap::new()),
        config_path: std::env::var("EXCEL_BRIDGE_ACCOUNT_MAP")?,
        suffix: std::env::var("EXCEL_BRIDGE_MODEL_SUFFIX").unwrap_or_else(|_| "-excel".into()),
        allow_unsigned: std::env::var("EXCEL_BRIDGE_ALLOW_UNSIGNED")
            .map(|v| v != "0" && v.to_lowercase() != "false")
            .unwrap_or(true),
    });
    upstream_error_log(&app).check()?;
    let router = Router::new()
        .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
        .route("/_control/{operation}", post(control))
        .route("/backend-api/codex/responses", post(http).get(ws))
        .route("/backend-api/codex/models", get(models))
        .layer(DefaultBodyLimit::max(request_body::MAX_BODY_BYTES))
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

#[cfg(test)]
mod terminal_error_tests {
    use super::*;
    #[tokio::test]
    async fn terminal_wrapper_ignores_errors_after_success() {
        let source: Events = Box::pin(futures_util::stream::iter(vec![
            Ok(json!({"type":"response.completed","sequence_number":8,"response":{"id":"resp_done","status":"completed","output":[]}})),
            Err("trailing transport failure"),
        ]));
        let output=terminal_errors(source).collect::<Vec<_>>().await;
        assert_eq!(output.len(),1);
        assert_eq!(output[0].as_ref().unwrap()["type"],"response.completed");
    }
    #[tokio::test]
    async fn terminal_wrapper_marks_eof_and_retains_identity_and_sequence() {
        let source: Events = Box::pin(futures_util::stream::iter(vec![
            Ok(json!({"type":"response.created","sequence_number":6,"response":{"id":"resp_open","status":"in_progress"}}))
        ]));
        let output=terminal_errors(source).collect::<Vec<_>>().await;
        assert_eq!(output.len(),2);
        let terminal=output[1].as_ref().unwrap();
        assert_eq!(terminal["type"],"response.failed");
        assert_eq!(terminal["response"]["id"],"resp_open");
        assert_eq!(terminal["sequence_number"],7);
    }
    #[tokio::test]
    async fn pooled_client_reuses_connection_and_does_not_bypass_changed_routes() {
        use axum::extract::ConnectInfo;
        use std::net::SocketAddr;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route(
                        "/",
                        get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move {
                            peer.to_string()
                        }),
                    )
                    .into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let root = std::env::temp_dir().join(format!("excel-client-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let config_path = root.join("accounts.json");
        std::fs::write(
            &config_path,
            r#"{"accounts":{"test":{"proxy":null,"direct":true}}}"#,
        )
        .unwrap();
        let app = App {
            secret: vec![0; 32],
            config_path: config_path.to_str().unwrap().into(),
            control: Arc::new(excel::control::Control::load(root.join("policy.json")).unwrap()),
            suffix: "-excel".into(),
            allow_unsigned: false,
            clients: Mutex::new(BTreeMap::new()),
        };
        let ctx = Context {
            account: "test".into(),
            scope: "test".into(),
            request_id: "test".into(),
            excel: true,
            key: None,
            expires: 0,
        };
        let url = format!("http://{address}/");
        let first = client(&app, &ctx)
            .unwrap()
            .get(&url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let second = client(&app, &ctx)
            .unwrap()
            .get(&url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(
            first, second,
            "sequential requests must reuse the TCP connection"
        );
        std::fs::write(
            &config_path,
            r#"{"accounts":{"test":{"proxy":null,"direct":false}}}"#,
        )
        .unwrap();
        assert!(
            client(&app, &ctx).is_err(),
            "changed invalid route must not reuse direct client"
        );
        std::fs::write(&config_path, r#"{"accounts":{}}"#).unwrap();
        assert!(
            client(&app, &ctx).is_err(),
            "revoked account must not reuse pooled client"
        );
        server.abort();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn partial_response_gets_failed_terminal_without_success_or_payload_leak() {
        let events: Events = Box::pin(futures_util::stream::iter(vec![
            Ok(
                json!({"type":"response.created","sequence_number":3,"response":{"id":"resp_test"}}),
            ),
            Err("invalid tool transport envelope"),
        ]));
        let output: Vec<_> = terminal_errors(events).collect().await;
        assert_eq!(output.len(), 2);
        let terminal = output[1].as_ref().unwrap();
        assert_eq!(terminal["type"], "response.failed");
        assert_eq!(terminal["sequence_number"], 4);
        assert_eq!(terminal["response"]["id"], "resp_test");
        assert_eq!(terminal["response"]["error"]["message"], "invalid tool transport envelope");
    }
}

#[cfg(test)]
mod account_route_tests {
    use super::*;

    #[test]
    fn unsigned_routing_matches_upstream_identity_and_rejects_ambiguity() {
        let config: Config = serde_json::from_value(json!({"accounts":{
            "acct_first":{"direct":true,"upstream_account_id":"upstream_first"},
            "acct_second":{"direct":false,"proxy":"socks5h://127.0.0.1:1080","upstream_account_id":"upstream_second"}
        }})).unwrap();
        assert_eq!(unsigned_route(&config, Some("upstream_second")).unwrap().proxy.as_deref(), Some("socks5h://127.0.0.1:1080"));
        assert!(unsigned_route(&config, Some("unknown")).is_err());
        assert!(unsigned_route(&config, Some("")).is_err());
        assert!(unsigned_route(&config, None).is_err());
        let mut ambiguous = config;
        ambiguous.accounts.get_mut("acct_first").unwrap().upstream_account_id = Some("upstream_second".into());
        assert!(unsigned_route(&ambiguous, Some("upstream_second")).is_err());
    }
    #[tokio::test]
    async fn unsigned_missing_identity_never_uses_first_account() {
        let path = std::env::temp_dir().join(format!("excel-route-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&path, br#"{"accounts":{"acct_one":{"direct":true}}}"#).unwrap();
        let app = App {
            secret: vec![7; 32],
            config_path: path.to_string_lossy().into_owned(),
            control: Arc::new(excel::control::Control::load(path.with_extension("policy")).unwrap()),
            suffix: "-excel".into(),
            allow_unsigned: true,
            clients: Mutex::new(BTreeMap::new()),
        };
        let result = unsigned_client(&app, None);
        std::fs::remove_file(path).unwrap();
        assert!(result.is_err(), "missing identity must never select the first account");
    }
}

#[cfg(test)]
mod policy_retry_http_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Mock {
        requests: Mutex<Vec<(Vec<u8>, String)>>,
        count: AtomicUsize,
        first_has_output: bool,
        successes_after: Option<usize>,
        reopen_http_error: bool,
    }
    async fn serve_mock(State(state): State<Arc<Mock>>, headers: HeaderMap, body: Bytes) -> Response {
        let count=state.count.fetch_add(1,Ordering::SeqCst)+1;
        state.requests.lock().unwrap().push((body.to_vec(),headers.get("authorization").unwrap().to_str().unwrap().into()));
        if count>1 && state.reopen_http_error { return StatusCode::SERVICE_UNAVAILABLE.into_response(); }
        let id=format!("response_{count}");
        let mut events=vec![json!({"type":"response.created","response":{"id":id,"status":"in_progress"}})];
        if state.first_has_output {events.push(json!({"type":"response.output_text.delta","delta":"already delivered"}));}
        if state.successes_after.is_some_and(|n|count>n) {
            events.push(json!({"type":"response.completed","response":{"id":id,"status":"completed","output":[]}}));
        } else {
            events.push(json!({"type":"error","error":{"code":"cyber_policy","type":"invalid_prompt","message":"private fixture text"}}));
        }
        let wire=events.iter().flat_map(excel::stream::encode).collect::<Vec<_>>();
        ([("content-type","text/event-stream")],wire).into_response()
    }
    async fn run_with_options(enabled:bool, normalize: bool, first_has_output:bool, successes_after:Option<usize>, reopen_http_error:bool) -> (Vec<Value>,Arc<Mock>,Value) {
        let folder=std::env::temp_dir().join(format!("cpr-retry-http-{}",uuid::Uuid::new_v4()));std::fs::create_dir(&folder).unwrap();
        let config=folder.join("accounts.json");std::fs::write(&config,json!({"accounts":{"fixture-account":{"proxy":null,"direct":true}}}).to_string()).unwrap();
        let control=Arc::new(excel::control::Control::load(folder.join("policy.json")).unwrap());
        control.save(excel::admission::Policy { enabled:true,retry_policy_errors:enabled,normalize_user_prompt:normalize,..Default::default() },Some(0)).unwrap();
        let app=Arc::new(App { secret:b"fixture-signing-key-minimum-32-bytes".to_vec(),config_path:config.to_string_lossy().into(),control:control.clone(),suffix:"-excel".into(),allow_unsigned:false,clients:Mutex::new(BTreeMap::new()) });
        let ctx=Context { account:"fixture-account".into(),scope:"fixture-scope".into(),request_id:uuid::Uuid::new_v4().to_string(),excel:true,key:None,expires:SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()+60 };
        let token=excel::auth::sign(&ctx,&app.secret).unwrap();
        let input=json!({"model":"gpt-test-excel","stream":true,"input":"Reply OK.","client_metadata":{"_cpr_excel_bridge":token}});
        let mut headers=HeaderMap::new();headers.insert("authorization","Bearer fixture-only".parse().unwrap());
        let state=Arc::new(Mock {requests:Mutex::new(Vec::new()),count:AtomicUsize::new(0),first_has_output,successes_after,reopen_http_error});
        let router=Router::new().route("/responses",post(serve_mock)).with_state(state.clone());
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint: &'static str=Box::leak(format!("http://{}/responses",listener.local_addr().unwrap()).into_boxed_str());
        let server=tokio::spawn(async move {axum::serve(listener,router).await.unwrap()});
        let mut stream=open_events_at(app,Identity::Signed(Box::new(ctx)),headers,input,None,endpoint).await.unwrap();
        let mut events=Vec::new();
        while let Some(event)=stream.next().await {events.push(event.unwrap());}
        drop(stream);server.abort();std::fs::remove_dir_all(folder).unwrap();
        let snapshot=control.snapshot();
        assert_eq!(snapshot["active"],0);
        assert_eq!(events.iter().filter(|e|excel::stream::is_terminal(e)).count(),1);
        assert!(!json!(events).to_string().contains("private fixture text"));
        let requests=state.requests.lock().unwrap();
        assert!(requests.iter().all(|r|r==&requests[0]),"retries must not rewrite body or change credentials");
        drop(requests);
        (events,state,snapshot)
    }
    async fn run(enabled:bool, first_has_output:bool, successes_after:Option<usize>, reopen_http_error:bool) -> (Vec<Value>,Arc<Mock>,Value) {
        run_with_options(enabled, false, first_has_output, successes_after, reopen_http_error).await
    }
    #[tokio::test]
    async fn disabled_returns_original_policy_without_retry() {
        let (events,state,_)=run(false,false,None,false).await;
        assert_eq!(state.count.load(Ordering::SeqCst),1);
        assert_eq!(events.last().unwrap()["response"]["error"]["code"],"cyber_policy");
    }
    #[tokio::test]
    async fn enabled_stops_after_five_retries_and_returns_policy() {
        let (events,state,snapshot)=run(true,false,None,false).await;
        assert_eq!(state.count.load(Ordering::SeqCst),6);
        assert_eq!(events[0]["response"]["id"],"response_6");
        assert_eq!(events.last().unwrap()["response"]["error"]["code"],"cyber_policy");
        assert_eq!(snapshot["records"][0]["error_code"],"cyber_policy");
    }
    #[tokio::test]
    async fn retry_success_exposes_only_the_successful_response() {
        let (events,state,_)=run(true,false,Some(1),false).await;
        assert_eq!(state.count.load(Ordering::SeqCst),2);
        assert_eq!(events[0]["response"]["id"],"response_2");
        assert_eq!(events.last().unwrap()["type"],"response.completed");
    }
    #[tokio::test]
    async fn normalize_prompt_is_applied_to_the_actual_excel_request_body() {
        let (_, state, _) = run_with_options(true, true, false, Some(1), false).await;
        let requests = state.requests.lock().unwrap();
        let body: Value = serde_json::from_slice(&requests[0].0).unwrap();
        let text = body.pointer("/input/1/content/0/text").and_then(Value::as_str).unwrap();
        assert!(text.starts_with(excel::prompt_normalization::PREFACE));
        assert!(text.ends_with("Reply OK."));
        assert_eq!(requests.len(), 2);
    }
    #[tokio::test]
    async fn delivered_output_prevents_retry_even_when_enabled() {
        let (events,state,_)=run(true,true,None,false).await;
        assert_eq!(state.count.load(Ordering::SeqCst),1);
        assert!(events.iter().any(|e|e["delta"]=="already delivered"));
        assert_eq!(events.last().unwrap()["type"],"response.failed");
    }
    #[tokio::test]
    async fn reopen_transport_failure_preserves_original_policy() {
        let (events,state,_)=run(true,false,None,true).await;
        assert_eq!(state.count.load(Ordering::SeqCst),2);
        assert_eq!(events[0]["response"]["id"],"response_1");
        assert_eq!(events.last().unwrap()["response"]["error"]["code"],"cyber_policy");
    }
}

#[cfg(test)]
mod complete_error_record_tests {
    use super::*;
    #[tokio::test]
    async fn records_before_stream_translation_and_http_truncation() {
        for status in [StatusCode::OK, StatusCode::BAD_REQUEST] {
            let root=std::env::temp_dir().join(format!("excel-error-record-{}",uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let map=root.join("accounts.json");
            std::fs::write(&map, r#"{"accounts":{"test":{"direct":true,"proxy":null}}}"#).unwrap();
            let app=Arc::new(App { secret:vec![b'x';32],config_path:map.to_string_lossy().into_owned(),control:Arc::new(excel::control::Control::load(root.join("policy.json")).unwrap()),suffix:"-excel".into(),allow_unsigned:false,clients:Mutex::new(BTreeMap::new()) });
            app.control.save(excel::admission::Policy {enabled:true,..Default::default()},Some(0)).unwrap();
            let message=format!("field input rejected: {} END_OF_FULL_ERROR", "detail ".repeat(2000));
            let event=json!({"type":"error","error":{"code":"new_provider_validation_error","type":"invalid_request_error","message":message,"param":"input[1]","details":{"constraint":"must be tool output"}}});
            let payload=if status.is_success() { format!("event: error\ndata: {event}\n\n") } else { event.to_string() };
            let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address=listener.local_addr().unwrap();
            let mock=Router::new().route("/",post(move || { let payload=payload.clone(); async move {(status,[("content-type","text/event-stream")],payload)} }));
            let server=tokio::spawn(async move { axum::serve(listener,mock).await.unwrap(); });
            let ctx=Context {account:"test".into(),scope:"error-record-test".into(),request_id:format!("req_record_{}",status.as_u16()),excel:true,key:None,expires:SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()+60};
            let token=excel::auth::sign(&ctx,&app.secret).unwrap();
            let source=json!({"model":"gpt-6-astra-excel","input":[{"role":"user","content":"diagnostic fixture"}],"client_metadata":{"_cpr_excel_bridge":token}});
            let mut headers=HeaderMap::new();headers.insert("authorization","Bearer fixture_account_credential".parse().unwrap());
            let endpoint: &'static str=Box::leak(format!("http://{address}/").into_boxed_str());
            let result=open_events_at(app,Identity::Signed(Box::new(ctx.clone())),headers,source,None,endpoint).await;
            if status.is_success() {
                let output=result.unwrap().collect::<Vec<_>>().await;
                assert!(output.iter().any(|e| e.as_ref().is_ok_and(|v|v["type"]=="response.failed")));
                assert!(!format!("{output:?}").contains("END_OF_FULL_ERROR"));
            } else { assert_eq!(result.err().unwrap().0,status); }
            let text=std::fs::read_to_string(root.join("error-logs/upstream-errors.jsonl")).unwrap();
            let record:Value=serde_json::from_str(text.trim()).unwrap();
            let saved=if status.is_success() {&record["upstream_event"]} else {&record["upstream_body"]};
            assert_eq!(saved["error"],event["error"]);
            assert_eq!(record["request_id"],ctx.request_id);
            assert_eq!(record["http_status"],status.as_u16());
            server.abort(); std::fs::remove_dir_all(root).unwrap();
        }
    }
}
