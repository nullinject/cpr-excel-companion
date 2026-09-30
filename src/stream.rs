//! 按完整 SSE 事件转换，保持背压；EOF 不伪造成功终态。
use super::{Result, Tools, restore_call};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_EVENT_BYTES: usize = 2 * 1024 * 1024;
#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
}
impl Decoder {
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Value>> {
        self.pending.extend_from_slice(chunk);
        let mut events = Vec::new();
        loop {
            let end = self
                .pending
                .windows(2)
                .position(|w| w == b"\n\n")
                .map(|p| (p, 2));
            let cr = self
                .pending
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|p| (p, 4));
            let boundary = match (end, cr) {
                (Some(a), Some(b)) => Some(if a.0 < b.0 { a } else { b }),
                (a, b) => a.or(b),
            };
            let Some((index, delimiter)) = boundary else {
                break;
            };
            if index > MAX_EVENT_BYTES {
                return Err("upstream SSE event is too large");
            }
            let frame: Vec<u8> = self.pending.drain(..index + delimiter).collect();
            let text = std::str::from_utf8(&frame).map_err(|_| "invalid SSE UTF-8")?;
            let data = text
                .lines()
                .filter_map(|l| {
                    l.strip_prefix("data:")
                        .map(|v| v.strip_prefix(' ').unwrap_or(v))
                })
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            events.push(serde_json::from_str(&data).map_err(|_| "invalid SSE JSON")?);
        }
        if self.pending.len() > MAX_EVENT_BYTES {
            return Err("upstream SSE event is too large");
        }
        Ok(events)
    }
    pub fn finish(&self) -> Result<()> {
        if self.pending.iter().all(u8::is_ascii_whitespace) {
            Ok(())
        } else {
            Err("truncated SSE event")
        }
    }
}

/// Only fixed, known error codes leave the bridge; raw upstream error text may
/// contain prompts or credentials. Preserve the classification so CPR can apply
/// its existing recovery policy without retrying a partially delivered response.
const ERROR_FIELDS: [&str; 8] = [
    "/response/error/code",
    "/error/code",
    "/code",
    "/error_code",
    "/response/error/type",
    "/error/type",
    "/error_type",
    "/response/error_type",
];

pub fn failure_details(event: &Value) -> (&'static str, &'static str, &'static str) {
    // An unfamiliar provider code must not hide a recognized error type.
    ERROR_FIELDS.into_iter()
        .filter_map(|path| event.pointer(path).and_then(Value::as_str))
        .find_map(known_failure)
        .unwrap_or(("excel_upstream_failed", "server_error",
            "Excel upstream returned an error without a recognized error code; the response did not complete"))
}

fn known_failure(code: &str) -> Option<(&'static str, &'static str, &'static str)> {
    Some(match code {
        // These are native Responses classifications, not transport failures.
        // Keep safety decisions fatal and return fixed text, never raw upstream messages.
        "cyber_policy" => (
            "cyber_policy",
            "invalid_request_error",
            "Excel upstream blocked this request under cyber policy",
        ),
        "bio_policy" => (
            "bio_policy",
            "invalid_request_error",
            "Excel upstream blocked this request under bio policy",
        ),
        "misalignment_policy_violation" => (
            "misalignment_policy_violation",
            "invalid_request_error",
            "Excel upstream blocked this request under misalignment policy",
        ),
        "invalid_prompt" => (
            "invalid_prompt",
            "invalid_request_error",
            "Excel upstream rejected the prompt",
        ),
        "content_filter" => (
            "content_filter",
            "invalid_request_error",
            "Excel upstream blocked this request under its content policy",
        ),
        "server_overloaded" => (
            "server_overloaded",
            "server_error",
            "Excel upstream overloaded",
        ),
        "usage_not_included" => (
            "usage_not_included",
            "usage_not_included",
            "Excel upstream usage is not included for this account",
        ),
        "rate_limit_exceeded" | "rate_limit_error" => (
            "rate_limit_exceeded",
            "rate_limit_error",
            "Excel upstream rate limit exceeded",
        ),
        "usage_limit_reached" => (
            "usage_limit_reached",
            "usage_limit_reached",
            "Excel upstream account usage limit reached",
        ),
        "insufficient_quota" => (
            "insufficient_quota",
            "insufficient_quota",
            "Excel upstream quota exhausted",
        ),
        "context_length_exceeded" => (
            "context_length_exceeded",
            "invalid_request_error",
            "Excel upstream context length exceeded; shorten or compact the conversation",
        ),
        "server_error" => (
            "server_error",
            "server_error",
            "Excel upstream server error",
        ),
        "internal_server_error" => (
            "internal_server_error",
            "server_error",
            "Excel upstream internal server error",
        ),
        "temporarily_unavailable" => (
            "temporarily_unavailable",
            "server_error",
            "Excel upstream temporarily unavailable",
        ),
        "overloaded" | "overloaded_error" => {
            ("overloaded", "server_error", "Excel upstream overloaded")
        }
        "invalid_request_error" => (
            "invalid_request_error",
            "invalid_request_error",
            "Excel upstream rejected the request",
        ),
        "invalid_api_key" => (
            "invalid_api_key",
            "authentication_error",
            "Excel upstream authentication failed",
        ),
        "authentication_error" => (
            "authentication_error",
            "authentication_error",
            "Excel upstream authentication failed",
        ),
        "permission_denied" | "permission_error" => (
            "permission_denied",
            "permission_error",
            "Excel upstream permission denied",
        ),
        "model_not_found" => (
            "model_not_found",
            "invalid_request_error",
            "Excel upstream model unavailable",
        ),
        "basispoints_model_access_changed" => (
            "basispoints_model_access_changed",
            "permission_error",
            "Excel upstream model access changed; select a verified available model",
        ),
        _ => return None,
    })
}

/// Optional public classification only. Call after recording the original error.
/// Never infer a policy code from arbitrary upstream message text.
pub fn policy_server_error(event: &Value) -> Option<Value> {
    let policy = matches!(
        failure_details(event).0,
        "cyber_policy" | "bio_policy" | "misalignment_policy_violation" | "content_filter"
    ) || (event["type"] == "response.incomplete"
        && incomplete_reason(event) == "content_filter");
    policy.then(|| {
        json!({
            "code": "server_error",
            "type": "server_error",
            "message": "Excel upstream server error"
        })
    })
}

/// Safe diagnostic shape only: no arbitrary codes, messages, prompts or credentials.
pub fn failure_diagnostics(event: &Value) -> Value {
    let mut fields = serde_json::Map::new();
    for path in ERROR_FIELDS.into_iter().chain([
        "/error",
        "/message",
        "/error/message",
        "/response/error",
        "/response/error/message",
    ]) {
        if let Some(value) = event.pointer(path) {
            let shape = match value {
                Value::Null => "null",
                Value::Bool(_) => "boolean",
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::Array(_) => "array",
                Value::Object(_) => "object",
            };
            let mut detail = json!({"shape": shape});
            if let Some((code, _, _)) = value.as_str().and_then(known_failure) {
                detail["recognized_code"] = json!(code);
            }
            fields.insert(path.into(), detail);
        }
    }
    json!(fields)
}

pub fn incomplete_reason(event: &Value) -> &'static str {
    match event
        .pointer("/response/incomplete_details/reason")
        .and_then(Value::as_str)
    {
        Some("max_output_tokens") => "max_output_tokens",
        Some("content_filter") => "content_filter",
        _ => "unknown",
    }
}
pub fn is_terminal(event: &Value) -> bool {
    matches!(
        event["type"].as_str(),
        Some("response.completed" | "response.failed" | "response.incomplete" | "error")
    )
}

pub struct Translator {
    tools: Tools,
    emitted: BTreeSet<String>,
    pub originals: BTreeMap<String, Value>,
    pub terminal: bool,
    pub completed: Option<Value>,
    sequence: u64,
    response_id: Option<String>,
}
impl Translator {
    pub fn new(tools: Tools) -> Self {
        Self {
            tools,
            emitted: BTreeSet::new(),
            originals: BTreeMap::new(),
            terminal: false,
            completed: None,
            sequence: 0,
            response_id: None,
        }
    }
    fn tool_events(&mut self, native: &Value, index: Value) -> Result<Vec<Value>> {
        let item = restore_call(native, &self.tools)?;
        let call_id = item["call_id"]
            .as_str()
            .ok_or("missing tool call id")?
            .to_owned();
        self.originals.insert(call_id.clone(), native.clone());
        if !self.emitted.insert(call_id) {
            return Ok(vec![]);
        }
        let custom = item["type"] == "custom_tool_call";
        let field = if custom { "input" } else { "arguments" };
        let base = if custom {
            "response.custom_tool_call_input"
        } else {
            "response.function_call_arguments"
        };
        let mut added = item.clone();
        added[field] = json!("");
        added["status"] = json!("in_progress");
        let mut done =
            json!({"type":format!("{base}.done"),"output_index":index,"item_id":item["id"]});
        done[field] = item[field].clone();
        Ok(vec![
            json!({"type":"response.output_item.added","output_index":index,"item":added}),
            json!({"type":format!("{base}.delta"),"output_index":index,"item_id":item["id"],"delta":item[field]}),
            done,
            json!({"type":"response.output_item.done","output_index":index,"item":item}),
        ])
    }
    pub fn event(&mut self, mut event: Value) -> Result<Vec<Value>> {
        if self.terminal {
            return Ok(vec![]);
        }
        let kind = event["type"]
            .as_str()
            .ok_or("missing SSE event type")?
            .to_owned();
        if let Some(id) = event.pointer("/response/id").and_then(Value::as_str) {
            self.response_id = Some(id.to_owned());
        }
        let mut out = match kind.as_str() {
            "error" | "response.failed" | "response.incomplete" => {
                self.terminal = true;
                let incomplete = kind == "response.incomplete";
                let id = self.response_id.as_deref().unwrap_or("resp_excel_failed");
                let mut response = json!({"id":id,"object":"response",
                    "status":if incomplete {"incomplete"} else {"failed"},"output":[]});
                // Preserve protocol accounting and partial non-tool output, never
                // turn unfinished tool arguments into an executable client call.
                for field in ["model", "created_at", "usage"] {
                    if let Some(value) = event["response"].get(field) {
                        response[field] = value.clone();
                    }
                }
                if let Some(items) = event.pointer("/response/output").and_then(Value::as_array) {
                    response["output"] = json!(
                        items
                            .iter()
                            .filter(|item| matches!(
                                item["type"].as_str(),
                                Some("message" | "reasoning")
                            ))
                            .cloned()
                            .collect::<Vec<_>>()
                    );
                }
                if incomplete {
                    response["error"] = Value::Null;
                    response["incomplete_details"] = json!({"reason":incomplete_reason(&event)});
                } else {
                    let (code, error_type, message) = failure_details(&event);
                    response["error"] = json!({"type":error_type,"code":code,"message":message});
                }
                let mut terminal = json!({"type":if incomplete {"response.incomplete"} else {"response.failed"},"response":response});
                if let Some(status) = event["status"].as_u64().filter(|v| (400..=599).contains(v)) {
                    terminal["status"] = json!(status);
                }
                vec![terminal]
            }
            "response.function_call_arguments.delta"
            | "response.function_call_arguments.done"
            | "response.custom_tool_call_input.delta"
            | "response.custom_tool_call_input.done" => vec![],
            "response.output_item.added"
                if matches!(
                    event["item"]["type"].as_str(),
                    Some("function_call" | "custom_tool_call")
                ) =>
            {
                vec![]
            }
            "response.output_item.done"
                if matches!(
                    event["item"]["type"].as_str(),
                    Some("function_call" | "custom_tool_call")
                ) =>
            {
                self.tool_events(&event["item"], event["output_index"].clone())?
            }
            "response.completed" => {
                let items = event
                    .pointer("/response/output")
                    .and_then(Value::as_array)
                    .ok_or("completed response has no output")?
                    .clone();
                let mut converted = Vec::new();
                let mut out = Vec::new();
                for (index, item) in items.into_iter().enumerate() {
                    if matches!(
                        item["type"].as_str(),
                        Some("function_call" | "custom_tool_call")
                    ) {
                        out.extend(self.tool_events(&item, json!(index))?);
                        converted.push(restore_call(&item, &self.tools)?);
                    } else {
                        converted.push(item);
                    }
                }
                event["response"]["output"] = json!(converted);
                self.completed = Some(event["response"].clone());
                self.terminal = true;
                out.push(event);
                out
            }
            _ => vec![event],
        };
        for event in &mut out {
            event["sequence_number"] = json!(self.sequence);
            self.sequence += 1;
        }
        Ok(out)
    }
}

pub fn encode(event: &Value) -> Vec<u8> {
    // SSE 注释：桥接保活心跳借道输出，所有合规解析器都忽略注释行。
    if event["type"] == "bridge.comment" {
        return format!(": {}\n\n", event["text"].as_str().unwrap_or("keepalive")).into_bytes();
    }
    format!(
        "event: {}\ndata: {}\n\n",
        event["type"].as_str().unwrap_or("error"),
        event
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decodes_split_unicode_and_crlf() {
        let data="event: response.output_text.delta\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"你好\"}\r\n\r\n".as_bytes();
        let mut decoder = Decoder::default();
        let mut out = vec![];
        for b in data {
            out.extend(decoder.push(&[*b]).unwrap());
        }
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["delta"], "你好");
        assert!(decoder.finish().is_ok());
    }
    #[test]
    fn truncated_stream_does_not_become_success() {
        let mut d = Decoder::default();
        d.push(b"data: {\"type\":").unwrap();
        assert!(d.finish().is_err());
    }
    #[test]
    fn error_has_one_terminal_and_does_not_echo_secrets() {
        let mut t = Translator::new(Tools::new());
        let out = t
            .event(json!({"type":"error","message":"private prompt secret"}))
            .unwrap();
        assert_eq!(out.len(), 1);
        assert!(!json!(out).to_string().contains("private prompt"));
        assert!(
            t.event(json!({"type":"response.failed"}))
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn failure_preserves_safe_code_usage_and_response_identity() {
        let mut t = Translator::new(Tools::new());
        let event = json!({"type":"response.failed","response":{
            "id":"resp_rate","model":"gpt-test","status":"failed","output":[],
            "usage":{"input_tokens":17,"output_tokens":2},
            "error":{"code":"rate_limit_exceeded","message":"secret prompt or bearer token"}}});
        let out = t.event(event).unwrap();
        assert_eq!(out[0]["response"]["error"]["code"], "rate_limit_exceeded");
        assert_eq!(out[0]["response"]["usage"]["input_tokens"], 17);
        assert_eq!(out[0]["response"]["id"], "resp_rate");
        assert!(!out[0].to_string().contains("secret prompt"));
    }
    #[test]
    fn incomplete_preserves_reason_and_partial_text_without_claiming_success() {
        let mut t = Translator::new(Tools::new());
        let event = json!({"type":"response.incomplete","response":{
            "id":"resp_limit","status":"incomplete",
            "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"partial"}]}],
            "incomplete_details":{"reason":"max_output_tokens"},
            "usage":{"input_tokens":11,"output_tokens":64}}});
        let out = t.event(event).unwrap();
        assert_eq!(out[0]["type"], "response.incomplete");
        assert_eq!(out[0]["response"]["status"], "incomplete");
        assert_eq!(
            out[0]["response"]["incomplete_details"]["reason"],
            "max_output_tokens"
        );
        assert_eq!(
            out[0]["response"]["output"][0]["content"][0]["text"],
            "partial"
        );
        assert_eq!(out[0]["response"]["usage"]["output_tokens"], 64);
        assert!(t.terminal);
        assert!(t.completed.is_none());
    }
    #[test]
    fn failed_terminal_without_response_uses_started_response_id() {
        let mut t = Translator::new(Tools::new());
        t.event(json!({"type":"response.created","response":{"id":"resp_started","status":"in_progress"}})).unwrap();
        let out = t
            .event(json!({"type":"error","error":{"code":"server_error","message":"private"}}))
            .unwrap();
        assert_eq!(out[0]["response"]["id"], "resp_started");
    }
    #[test]
    fn failed_and_incomplete_never_emit_partial_tool_calls_or_raw_error_fields() {
        for kind in ["response.failed", "response.incomplete"] {
            let mut t = Translator::new(Tools::new());
            let out=t.event(json!({"type":kind,"status":429,"message":"private_top",
                "error":{"code":"sk-private-token","message":"private_error"},
                "response":{"id":"resp_partial","status":"incomplete",
                "incomplete_details":{"reason":"private_reason"},"metadata":{"private":"private_metadata"},
                "output":[{"type":"function_call","name":"run_officejs","arguments":"{unfinished private_args"}]}})).unwrap();
            assert_eq!(out.len(), 1);
            assert_eq!(out[0]["response"]["output"], json!([]));
            assert!(!out[0].to_string().contains("private"));
            assert_eq!(out[0]["status"], 429);
            assert!(t.originals.is_empty());
            assert!(t.completed.is_none());
        }
    }
    #[test]
    fn top_level_error_type_is_classified_when_code_is_null() {
        let event =
            json!({"type":"error","error":{"code":null,"type":"server_error","message":"private"}});
        assert_eq!(failure_details(&event).0, "server_error");
        assert!(is_terminal(&event));
        assert!(!is_terminal(&json!({"type":"response.output_text.delta"})));
    }
    #[test]
    fn failure_uses_recognized_type_after_unknown_code_and_supports_flat_errors() {
        for event in [
            json!({"type":"error","error":{"code":"provider_failure_v2","type":"rate_limit_error"}}),
            json!({"type":"error","error_type":"rate_limit_exceeded"}),
            json!({"type":"error","error_code":"rate_limit_exceeded"}),
        ] {
            assert_eq!(failure_details(&event).0, "rate_limit_exceeded");
        }
        let event =
            json!({"type":"error","error":{"code":"model_not_found","type":"server_error"}});
        assert_eq!(failure_details(&event).0, "model_not_found");
    }
    #[test]
    fn diagnostics_explain_shape_without_echoing_private_fields() {
        let event = json!({"type":"error","message":"private prompt", "error":{"code":"private-token","type":"rate_limit_error","message":"Bearer private-token"}});
        let diagnostic = failure_diagnostics(&event);
        assert_eq!(diagnostic["/error/code"]["shape"], "string");
        assert_eq!(
            diagnostic["/error/type"]["recognized_code"],
            "rate_limit_exceeded"
        );
        assert!(!diagnostic.to_string().contains("private"));
        assert!(!diagnostic.to_string().contains("Bearer"));
    }
    #[test]
    fn model_access_changed_keeps_specific_code_without_private_message_or_policy_remap() {
        let event = json!({"error":{"code":"basispoints_model_access_changed","type":"invalid_request_error","message":"private workspace and credential"}});
        let (code, category, message) = failure_details(&event);
        assert_eq!(code, "basispoints_model_access_changed");
        assert_eq!(category, "permission_error");
        assert!(!message.contains("private"));
        assert!(policy_server_error(&event).is_none());
    }
    #[test]
    fn tool_call_is_emitted_once_and_preserves_original() {
        let source = json!({"tools":[{"type":"custom","name":"apply_patch"}]});
        let mut t = Translator::new(super::super::tool_catalog(&source));
        let native = json!({"type":"function_call","id":"fc_native","call_id":"call_1","name":"run_officejs","arguments":json!({"code":"*** Begin Patch\n*** End Patch","references":["apply_patch"]}).to_string()});
        assert!(
            t.event(json!({"type":"response.output_item.added","output_index":0,"item":native}))
                .unwrap()
                .is_empty()
        );
        let out = t
            .event(json!({"type":"response.output_item.done","output_index":0,"item":native}))
            .unwrap();
        assert_eq!(out.len(), 4);
        assert_eq!(out[3]["item"]["type"], "custom_tool_call");
        let end=t.event(json!({"type":"response.completed","response":{"id":"resp_1","output":[native],"usage":{"input_tokens":10}}})).unwrap();
        assert_eq!(end.len(), 1);
        assert_eq!(t.originals["call_1"]["id"], "fc_native");
        assert_eq!(t.completed.as_ref().unwrap()["usage"]["input_tokens"], 10);
    }
}
