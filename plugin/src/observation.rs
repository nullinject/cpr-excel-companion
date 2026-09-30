use cpr_excel_companion::control::Control;
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::observation::{Event, RequestCompleted},
    client::{Empty, TypedCall, TypedReply},
};

pub async fn handle(
    call: TypedCall<Event>,
    control: &Control,
) -> Result<TypedReply<Empty>, PluginFault> {
    let Event::RequestCompleted(observation) = call.request else {
        return Ok(TypedReply::new(Empty {}));
    };
    let payload = project_completion(&observation)?;
    let observation = serde_json::from_slice(&payload)
        .map_err(|_| PluginFault::new(ErrorCode::InvalidInput, "invalid observation"))?;
    control.observe(&observation, crate::EXCEL_SUFFIX);
    Ok(TypedReply::new(Empty {}))
}

fn project_completion(observation: &RequestCompleted) -> Result<Vec<u8>, PluginFault> {
    let mut projected = serde_json::to_value(observation)
        .map_err(|_| PluginFault::new(ErrorCode::InvalidInput, "observation encoding failed"))?;
    // 桥接旧合同的顶层 failure 由新版完成事件中的 usage.failure 投影。
    projected["failure"] = projected["usage"]["failure"].clone();
    serde_json::to_vec(&projected)
        .map_err(|_| PluginFault::new(ErrorCode::InvalidInput, "observation encoding failed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_new_completion_into_existing_bridge_contract() {
        let event: Event = serde_json::from_value(serde_json::json!({
            "event": "request_completed", "data": {
                "event_id": "obs-1", "request_id": "req-1", "config_revision": 1,
                "operation": "responses", "client_key_id": "key-1", "account_id": "account-1",
                "completed_at_ms": 1000,
                "terminal": {"outcome":"failed", "send_state":"sent", "attempt_count":1, "error_code":"rate_limit_exceeded"},
                "usage": {"input_tokens":20,"output_tokens":3,"total_tokens":23,
                    "timings":{"first_token_ms":10,"latency_ms":100},
                    "failure":{"outcome":"failed","send_state":"sent","attempt_count":1,"upstream_status_code":429,"error_code":"rate_limit_exceeded"}}
            }
        })).unwrap();
        let Event::RequestCompleted(completed) = event else {
            panic!("wrong event")
        };
        let projected: cpr_excel_companion::observe::ObserveEvent =
            serde_json::from_slice(&project_completion(&completed).unwrap()).unwrap();
        assert_eq!(projected.client_key_id.as_deref(), Some("key-1"));
        assert_eq!(projected.failure.unwrap().upstream_status_code, Some(429));
        assert_eq!(
            projected.terminal.unwrap().outcome,
            cpr_excel_companion::observe::Outcome::Failed
        );
        let usage = projected.usage.unwrap();
        assert_eq!(usage.total_tokens, Some(23));
        assert_eq!(usage.timings.unwrap().first_token_ms, Some(10));
    }
}
