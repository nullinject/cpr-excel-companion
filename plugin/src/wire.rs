//! Project translated Responses events into host facts without duplicating client wire.
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        model::{
            CanonicalEvent as Fact, ContentKind, ExecutionEvent, FinishReason, Usage, WireEvent,
            WirePayload,
        },
        upstream_adapter::{UpstreamAdapterEvent, UpstreamFailure, UpstreamFailureKind},
    },
};
use serde_json::Value;
use std::collections::BTreeMap;
#[derive(Default)]
pub struct Projection {
    started: bool,
    contents: BTreeMap<u32, ContentKind>,
    tools: BTreeMap<u32, (String, String)>,
}
pub fn fault(message: &str) -> PluginFault {
    eprintln!("excel_adapter: {message}");
    PluginFault::new(ErrorCode::InvalidInput, message)
}
pub fn failure(status: u16, message: &str, code: &str) -> UpstreamAdapterEvent {
    let kind = match status {
        400 | 422 => UpstreamFailureKind::InvalidRequest,
        401 => UpstreamFailureKind::Unauthorized,
        403 => UpstreamFailureKind::PermissionDenied,
        429 => UpstreamFailureKind::RateLimited,
        408 | 504 => UpstreamFailureKind::Timeout,
        _ => UpstreamFailureKind::Unavailable,
    };
    let mut frame = UpstreamAdapterEvent::new(ExecutionEvent::default());
    frame.failure = Some(UpstreamFailure {
        kind,
        status: Some(status),
        retry_after_ms: None,
        message: message.into(),
        code: Some(code.into()),
    });
    frame
}
pub fn http_failure(status: u16, body: &[u8], map_policy: bool) -> UpstreamAdapterEvent {
    let parsed: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    if map_policy && cpr_excel_companion::stream::policy_server_error(&parsed).is_some() {
        return failure(500, "Excel upstream server error", "server_error");
    }
    let (code, category, message) = cpr_excel_companion::stream::failure_details(&parsed);
    if code == "excel_upstream_failed" {
        return failure(status, "Upstream rejected request", "upstream_rejected");
    }
    let mut frame = failure(status, message, code);
    if matches!(
        category,
        "usage_limit_reached" | "insufficient_quota" | "usage_not_included"
    ) {
        frame.failure.as_mut().unwrap().kind = UpstreamFailureKind::QuotaExhausted;
    }
    frame
}
fn usage(value: &Value) -> Usage {
    Usage {
        input_tokens: value["input_tokens"].as_u64(),
        output_tokens: value["output_tokens"].as_u64(),
        total_tokens: value["total_tokens"].as_u64(),
        cached_tokens: value["input_tokens_details"]["cached_tokens"].as_u64(),
        cache_write_tokens: value["input_tokens_details"]["cache_write_tokens"].as_u64(),
        reasoning_tokens: value["output_tokens_details"]["reasoning_tokens"].as_u64(),
        image_input_tokens: value["input_tokens_details"]["image_tokens"].as_u64(),
        image_output_tokens: value["output_tokens_details"]["image_tokens"].as_u64(),
    }
}
impl Projection {
    /// Failure envelopes cannot contain canonical facts. Publish observed usage first.
    pub fn failure_usage(&self, event: &Value) -> Option<UpstreamAdapterEvent> {
        let value = &event["response"]["usage"];
        (self.started
            && value.is_object()
            && matches!(
                event["type"].as_str(),
                Some("response.failed" | "response.incomplete")
            ))
        .then(|| {
            UpstreamAdapterEvent::new(ExecutionEvent::canonical(Fact::Usage {
                usage: usage(value),
            }))
        })
    }
    pub fn event(&mut self, event: Value) -> Result<UpstreamAdapterEvent, PluginFault> {
        let kind = event["type"]
            .as_str()
            .ok_or_else(|| fault("upstream event has no type"))?;
        let response = &event["response"];
        let model = response["model"].as_str().map(str::to_owned);
        let mut facts = vec![];
        let mut frame = UpstreamAdapterEvent::new(ExecutionEvent::default());
        if matches!(kind, "error" | "response.failed" | "response.incomplete") {
            let (code, category, message) = cpr_excel_companion::stream::failure_details(&event);
            let status = match category {
                "invalid_request_error" => 400,
                "authentication_error" => 401,
                "permission_error" => 403,
                "rate_limit_error"
                | "usage_limit_reached"
                | "insufficient_quota"
                | "usage_not_included" => 429,
                _ => 502,
            };
            frame = failure(status, message, code);
            if matches!(
                category,
                "usage_limit_reached" | "insufficient_quota" | "usage_not_included"
            ) {
                frame.failure.as_mut().unwrap().kind = UpstreamFailureKind::QuotaExhausted;
            }
        } else {
            if !self.started {
                if let Some(id) = response["id"].as_str() {
                    facts.push(Fact::Started {
                        id: id.into(),
                        model: model.clone(),
                    });
                    self.started = true;
                } else if matches!(
                    kind,
                    "response.output_text.delta"
                        | "response.reasoning_summary_text.delta"
                        | "response.reasoning_text.delta"
                        | "response.output_item.added"
                        | "response.completed"
                ) {
                    return Err(fault("upstream content before response start"));
                }
            }
            let index = event["output_index"].as_u64().unwrap_or(0);
            let index = u32::try_from(index).map_err(|_| fault("invalid output index"))?;
            let item = &event["item"];
            if kind == "response.output_item.added"
                && matches!(
                    item["type"].as_str(),
                    Some("function_call" | "custom_tool_call")
                )
            {
                let id = item["call_id"]
                    .as_str()
                    .or(item["id"].as_str())
                    .ok_or_else(|| fault("tool call has no ID"))?
                    .to_owned();
                let name = item["name"]
                    .as_str()
                    .ok_or_else(|| fault("tool call has no name"))?
                    .to_owned();
                self.content(index, ContentKind::ToolCall, &mut facts)?;
                self.tools.insert(index, (id.clone(), name.clone()));
                facts.push(Fact::ToolCallDelta {
                    index,
                    id,
                    name: Some(name),
                    arguments: String::new(),
                });
            }
            match kind {
                "response.output_text.delta" => {
                    self.content(index, ContentKind::Text, &mut facts)?;
                    facts.push(Fact::TextDelta {
                        index,
                        text: event["delta"]
                            .as_str()
                            .ok_or_else(|| fault("invalid text delta"))?
                            .into(),
                    });
                }
                "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                    self.content(index, ContentKind::Reasoning, &mut facts)?;
                    facts.push(Fact::ReasoningDelta {
                        index,
                        text: event["delta"]
                            .as_str()
                            .ok_or_else(|| fault("invalid reasoning delta"))?
                            .into(),
                    });
                }
                "response.function_call_arguments.delta"
                | "response.custom_tool_call_input.delta" => {
                    let (id, name) = self
                        .tools
                        .get(&index)
                        .ok_or_else(|| fault("tool delta before tool start"))?;
                    facts.push(Fact::ToolCallDelta {
                        index,
                        id: id.clone(),
                        name: Some(name.clone()),
                        arguments: event["delta"]
                            .as_str()
                            .ok_or_else(|| fault("invalid tool delta"))?
                            .into(),
                    });
                }
                "response.completed" => {
                    let u = &response["usage"];
                    if u.is_object() {
                        facts.push(Fact::Usage { usage: usage(u) });
                    }
                    facts.push(Fact::Completed {
                        id: response["id"]
                            .as_str()
                            .ok_or_else(|| fault("completion has no ID"))?
                            .into(),
                        model: model.clone(),
                        reason: if self.tools.is_empty() {
                            FinishReason::Stop
                        } else {
                            FinishReason::ToolCall
                        },
                    });
                }
                _ => {}
            }
        }
        frame.event.facts = facts;
        frame.service_tier = response["service_tier"].as_str().map(str::to_owned);
        frame.event.wire = Some(WireEvent {
            protocol: "openai".into(),
            payload: WirePayload::Json {
                event: Some(kind.into()),
                data: event,
                id: None,
                retry: None,
                raw_sse: None,
            },
        });
        Ok(frame)
    }
    fn content(
        &mut self,
        index: u32,
        kind: ContentKind,
        facts: &mut Vec<Fact>,
    ) -> Result<(), PluginFault> {
        if let Some(existing) = self.contents.get(&index) {
            if *existing != kind {
                return Err(fault("output content kind changed"));
            }
        } else {
            self.contents.insert(index, kind);
            facts.push(Fact::ContentAdded { index, kind });
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn projects_usage_without_changing_wire() {
        let mut p = Projection::default();
        p.event(json!({"type":"response.created","response":{"id":"resp_1","model":"test"}}))
            .unwrap();
        let delta = p
            .event(json!({"type":"response.output_text.delta","output_index":0,"delta":"hello"}))
            .unwrap();
        assert_eq!(delta.event.facts.len(), 2);
        let wire = json!({"type":"response.completed","response":{"id":"resp_1","model":"test","usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5}}});
        let frame = p.event(wire.clone()).unwrap();
        assert!(
            matches!(&frame.event.facts[0], Fact::Usage { usage } if usage.total_tokens == Some(5))
        );
        assert!(
            matches!(frame.event.wire.unwrap().payload, WirePayload::Json { data, .. } if data == wire)
        );
    }
    #[test]
    fn preserves_failure_classification_for_host_recovery() {
        for (code, expected) in [
            (
                "context_length_exceeded",
                UpstreamFailureKind::InvalidRequest,
            ),
            ("invalid_api_key", UpstreamFailureKind::Unauthorized),
            ("insufficient_quota", UpstreamFailureKind::QuotaExhausted),
            ("cyber_policy", UpstreamFailureKind::InvalidRequest),
            ("server_error", UpstreamFailureKind::Unavailable),
        ] {
            let frame = Projection::default()
                .event(json!({"type":"response.failed","response":{"error":{"code":code}}}))
                .unwrap();
            assert_eq!(frame.failure.unwrap().kind, expected);
        }
    }
    #[test]
    fn metadata_can_precede_created_without_starting_a_response() {
        let mut p = Projection::default();
        for kind in [
            "response.metadata",
            "codex.rate_limits",
            "codex.response.metadata",
        ] {
            let frame = p.event(json!({"type":kind})).unwrap();
            assert!(frame.event.facts.is_empty());
            assert!(frame.event.wire.is_some());
        }
        let frame = p
            .event(json!({"type":"response.created","response":{"id":"resp_metadata"}}))
            .unwrap();
        assert!(matches!(&frame.event.facts[0], Fact::Started { id, .. } if id == "resp_metadata"));
    }
    #[test]
    fn http_errors_preserve_codes_without_echoing_private_payloads() {
        let body = br#"{"error":{"code":"insufficient_quota","message":"private prompt"}}"#;
        let error = http_failure(429, body, false).failure.unwrap();
        assert_eq!(error.kind, UpstreamFailureKind::QuotaExhausted);
        assert!(!error.message.contains("private prompt"));
        let body = br#"{"error":{"code":"cyber_policy"}}"#;
        assert_eq!(
            http_failure(400, body, false).failure.unwrap().status,
            Some(400)
        );
        assert_eq!(
            http_failure(400, body, true).failure.unwrap().status,
            Some(500)
        );
    }
    #[test]
    fn partial_failure_usage_is_separate_from_failure_envelope() {
        let mut p = Projection::default();
        let event = json!({"type":"response.failed","response":{"id":"resp_usage","error":{"code":"server_error"},"usage":{"input_tokens":10,"output_tokens":2,"input_tokens_details":{"cache_write_tokens":4}}}});
        assert!(p.failure_usage(&event).is_none());
        p.event(json!({"type":"response.created","response":{"id":"resp_usage"}}))
            .unwrap();
        let usage = p.failure_usage(&event).unwrap();
        assert!(
            matches!(&usage.event.facts[0], Fact::Usage { usage } if usage.output_tokens == Some(2) && usage.cache_write_tokens == Some(4))
        );
        assert!(p.event(event).unwrap().event.facts.is_empty());
    }
    #[test]
    fn failure_never_claims_success_or_usage() {
        let frame = Projection::default().event(json!({"type":"response.failed","response":{"error":{"code":"rate_limit_exceeded"}}})).unwrap();
        assert!(frame.event.facts.is_empty());
        assert!(frame.failure.is_some());
        assert!(
            Projection::default()
                .event(json!({"type":"response.output_text.delta","delta":"bad"}))
                .is_err()
        );
    }
}
