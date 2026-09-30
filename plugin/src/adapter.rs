//! Terminal adapter. All outbound traffic uses the host-selected account and proxy.
use crate::wire::{Projection, failure, fault};
use cpr_excel_companion::{self as excel, auth::Context, control::Control};
use futures_util::{Stream, StreamExt};
use gateway_plugin_sdk::{
    PluginFault,
    call::upstream_adapter::*,
    client::{
        Empty, HostClient, PullResponseFuture, PullResponseStream, ResponseStream, TypedCall,
        TypedReply, UpstreamWebSocketUpgrade,
    },
};
use serde_json::{Value, json};
use std::{pin::Pin, sync::Arc};
const NATIVE: &str = "https://chatgpt.com/backend-api/codex/responses";
pub fn registration() -> UpstreamAdapterRegistration {
    UpstreamAdapterRegistration {
        adapters: vec![UpstreamAdapterDeclaration {
            id: "excel-companion".into(),
            provider: BuiltinProvider::OpenAi,
            base_url: "https://bps.openai.com/basispoints/api/".into(),
            paths: vec![
                UpstreamPath {
                    path: "responses".into(),
                    purpose: UpstreamPathPurpose::Inference,
                },
                UpstreamPath {
                    path: "attachments".into(),
                    purpose: UpstreamPathPurpose::Auxiliary,
                },
            ],
            authentication_kinds: vec!["oauth".into()],
            transport: UpstreamTransport::WebSocket,
            protocol: "openai".into(),
            models: vec![],
        }],
    }
}
type Events = Pin<Box<dyn Stream<Item = Result<Vec<u8>, PluginFault>> + Send>>;
struct Pull(Events);
impl PullResponseStream for Pull {
    fn next(&mut self) -> PullResponseFuture<'_> {
        Box::pin(self.0.next())
    }
}
fn encode(event: UpstreamAdapterEvent) -> Result<Vec<u8>, PluginFault> {
    event
        .encode()
        .map_err(|_| fault("upstream event encoding failed"))
}
fn scope(call: &TypedCall<UpstreamAdapterRequest>) -> String {
    json!([
        call.context.instance_id,
        call.context.generation,
        call.context.incarnation,
        call.request.client_key_id,
        call.request.account_id,
        call.request.credential_revision
    ])
    .to_string()
}
fn headers(excel: bool, request: &UpstreamAdapterRequest) -> Vec<(String, String)> {
    // Never forward client credentials or allow client headers to override host identity.
    let mut headers = vec![
        ("content-type".into(), "application/json".into()),
        ("accept".into(), "text/event-stream".into()),
    ];
    for (name, bytes) in &request.headers {
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "session-id" | "conversation-id" | "openai-beta" | "originator" | "version"
        ) && let Ok(value) = std::str::from_utf8(bytes)
        {
            headers.push((name.clone(), value.into()));
        }
    }
    if excel {
        for (name, value) in EXCEL_HEADERS {
            headers.push(((*name).into(), (*value).into()));
        }
    }
    headers
}
const EXCEL_HEADERS: &[(&str, &str)] = &[
    ("user-agent", "Mozilla/5.0"),
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
];
async fn upload(
    host: &HostClient,
    headers: &[(String, String)],
    source: &mut Value,
) -> Result<(), PluginFault> {
    excel::attachments::upload_with(source, |content_type, body| {
        let mut headers = headers.to_vec();
        headers.retain(|(name, _)| name != "content-type" && name != "accept");
        headers.push(("content-type".into(), content_type));
        async move {
            let response = host
                .upstream_http(
                    UpstreamHttpRequest {
                        method: "POST".into(),
                        path: "attachments".into(),
                        query: vec![],
                        headers,
                    },
                    body,
                )
                .await
                .map_err(|_| excel::BridgeError::ExcelRequest("attachment connection failed"))?;
            if !(200..300).contains(&response.status) {
                return Err(excel::BridgeError::ExcelRequest(
                    "attachment upload rejected",
                ));
            }
            response
                .body
                .collect(65536)
                .await
                .map_err(|_| excel::BridgeError::ExcelRequest("invalid attachment response"))
        }
    })
    .await
    .map_err(|_| fault("attachment upload failed"))
}
pub async fn execute(
    call: TypedCall<UpstreamAdapterRequest>,
    control: Arc<Control>,
) -> Result<TypedReply<Empty>, PluginFault> {
    // Pull ownership ties cancellation/backpressure to the request, with no detached producer.
    let stream = async_stream::try_stream! {
        let mut source: Value = serde_json::from_slice(&call.payload).map_err(|_| fault("invalid request JSON"))?;
        let model = source["model"].as_str().unwrap_or(&call.request.upstream_model).to_owned();
        let context = Context { account: call.request.account_id.clone(), scope: call.request.client_key_id.clone(), request_id: call.context.request_id.clone().unwrap_or_else(|| call.context.resource_scope_id.clone()), excel: model.strip_suffix(crate::EXCEL_SUFFIX).is_some_and(|base| !base.is_empty()), key: None, expires: 0 };
        let lease = control.enter(&context, &model, crate::EXCEL_SUFFIX).await;
        if lease.rejected { yield encode(failure(429,"Excel concurrency or queue limit reached","excel_queue_full"))?; return; }
        let scope = scope(&call);
        let channel = if lease.excel { "excel" } else { "native" };
        if let Some(continuation) = &call.request.continuation {
            if continuation.state["channel"] != channel {
                yield encode(failure(400,"Channel changed; resend full input history","previous_response_not_found"))?; return;
            }
            source["previous_response_id"] = json!(continuation.upstream_response_id);
        } else if source.get("previous_response_id").is_some_and(|v| !v.is_null()) {
            yield encode(failure(400,"Continuation unavailable; resend full input history","previous_response_not_found"))?; return;
        }
        let mut projection = Projection::default();
        if lease.excel && source["generate"] == false {
            let events = excel::history::prewarm(&scope, &source).map_err(fault)?;
            for event in events { let done = event["type"] == "response.completed"; let mut frame = match projection.event(event.clone()) {
                    Ok(frame) => frame,
                    Err(error) => { yield encode(failure(502, &error.message, "adapter_projection_error"))?; return; }
                };
                if done { lease.terminal(&event); frame.continuation = continuation(&event, channel, false); }
                yield encode(frame)?;
            }
            return;
        }
        let object = source.as_object_mut().ok_or_else(|| fault("request must be an object"))?;
        object.remove("type");
        object.insert("model".into(), json!(model.strip_suffix(crate::EXCEL_SUFFIX).filter(|m| !m.is_empty()).unwrap_or(&model)));
        object.insert("stream".into(), json!(true));
        let headers = headers(lease.excel, &call.request);
        let input;
        let tools;
        let body;
        if lease.excel {
            if let Err(error) = excel::history::restore(&scope, &mut source) {
                lease.fail(error);
                yield encode(failure(400,error,if matches!(error, excel::history::MISSING | excel::history::CACHE_LIMIT) { "previous_response_not_found" } else { "invalid_request" }))?; return;
            }
            input = excel::history::input(&source).map_err(fault)?;
            upload(&call.host, &headers, &mut source).await?;
            tools = excel::tool_catalog(&source);
            body = excel::prepare(&source, &excel::cache_load(&scope)).map_err(fault)?;
        } else { input = vec![]; tools = Default::default(); body = source; }
        let native_ws = !lease.excel && call.request.client_transport == "websocket";
        let mut incoming: Pin<Box<dyn Stream<Item=Result<Value, PluginFault>> + Send>>;
        if native_ws {
            let upgrade = call.host.upstream_websocket(UpstreamWebSocketRequest { path: NATIVE.into(), headers, query: vec![] }).await?;
            let connection = match upgrade {
                UpstreamWebSocketUpgrade::Connected { connection, .. } => connection,
                UpstreamWebSocketUpgrade::Rejected { status, .. } => { yield encode(failure(status,"Native WebSocket handshake rejected","upstream_rejected"))?; return; }
            };
            let mut body = body;
            body["type"] = json!("response.create"); body.as_object_mut().unwrap().remove("stream");
            connection.send(WebSocketMessageKind::Text, serde_json::to_vec(&body).map_err(|_| fault("invalid request"))?).await?;
            incoming = Box::pin(async_stream::try_stream! {
                while let Some((_, bytes)) = connection.read().await? {
                    let event: Value = serde_json::from_slice(&bytes).map_err(|_| fault("invalid native WebSocket event"))?;
                    let done = excel::stream::is_terminal(&event) || event["type"] == "error";
                    yield event;
                    if done { return; }
                }
            });
        } else {
            if !lease.excel && body.get("previous_response_id").is_some_and(|v| !v.is_null()) {
                yield encode(failure(400,"Native HTTP requires full history","previous_response_not_found"))?; return;
            }
            let response = call.host.upstream_http(UpstreamHttpRequest { method: "POST".into(), path: if lease.excel { "responses" } else { NATIVE }.into(), headers, query: vec![] }, serde_json::to_vec(&body).map_err(|_| fault("invalid request"))?).await?;
            if !(200..300).contains(&response.status) {
                let status = response.status;
                let detail = response.body.collect(65536).await.unwrap_or_default();
                yield encode(crate::wire::http_failure(status, &detail, lease.excel && control.policy_errors_as_server_error()))?; return;
            }
            incoming = Box::pin(async_stream::try_stream! {
                let mut body = response.body;
                let mut decoder = excel::stream::Decoder::default();
                while let Some(bytes) = body.read().await? {
                    for event in decoder.push(&bytes).map_err(fault)? { yield event; }
                }
                decoder.finish().map_err(fault)?;
            });
        }
        let mut translator = excel::stream::Translator::new(tools);
        while let Some(event) = incoming.next().await {
            let event = match event { Ok(event) => event, Err(_) => {
                lease.fail("managed upstream stream failed");
                yield encode(failure(502,"Managed upstream stream failed","upstream_stream_error"))?; return;
            } };
            let events = if lease.excel { match translator.event(event) {
                Ok(events) => events, Err(error) => { lease.fail(error); yield encode(failure(502,error,"excel_translation_error"))?; return; }
            } } else { vec![event] };
            for mut event in events {
                if lease.excel && control.policy_errors_as_server_error()
                    && matches!(event["type"].as_str(), Some("response.failed" | "response.incomplete"))
                    && let Some(error) = excel::stream::policy_server_error(&event) {
                        event["type"] = json!("response.failed"); event["response"]["status"] = json!("failed"); event["response"]["error"] = error;
                        event["response"].as_object_mut().unwrap().remove("incomplete_details");
                    }
                if let Some(usage) = projection.failure_usage(&event) { yield encode(usage)?; }
                let terminal = excel::stream::is_terminal(&event) || event["type"] == "error";
                let mut frame = match projection.event(event.clone()) {
                    Ok(frame) => frame,
                    Err(error) => { yield encode(failure(502, &error.message, "adapter_projection_error"))?; return; }
                };
                if terminal {
                    lease.terminal(&event);
                    if lease.excel { excel::history::save(&scope,&input,&event["response"]); excel::cache_save(&scope,&translator.originals); }
                    if event["type"] == "response.completed" && (lease.excel || native_ws) { frame.continuation = continuation(&event,channel,native_ws); }
                }
                yield encode(frame)?;
                if terminal { return; }
            }
        }
        lease.fail("upstream ended without a terminal event");
        yield encode(failure(502,"Upstream ended without a terminal event","upstream_incomplete"))?;
    };
    Ok(TypedReply::new(Empty {})
        .with_stream(ResponseStream::pull(Box::new(Pull(Box::pin(stream))))))
}
fn continuation(event: &Value, channel: &str, websocket: bool) -> Option<UpstreamContinuation> {
    Some(UpstreamContinuation {
        scope: if websocket {
            ContinuationScope::ConnectionLocal
        } else {
            ContinuationScope::Persisted
        },
        upstream_response_id: event["response"]["id"].as_str()?.into(),
        state: json!({"channel":channel}).as_object()?.clone(),
    })
}
