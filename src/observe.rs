//! 宿主观察事件的宽容投影；只取记录需要的字段，多余字段忽略。
//! 与 SDK `ObserveRequest` 的 serde 合同保持兼容，桥接不依赖 SDK。
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Succeeded,
    Failed,
    Rejected,
    Cancelled,
    Incomplete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Terminal {
    pub outcome: Outcome,
    #[serde(default)]
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Failure {
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub upstream_status_code: Option<u16>,
    #[serde(default)]
    pub client_status_code: Option<u16>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cached_tokens: Option<u64>,
    #[serde(default)]
    pub cache_write_tokens: Option<u64>,
    #[serde(default)]
    pub reasoning_tokens: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObserveEvent {
    pub event_id: String,
    pub request_id: String,
    #[serde(default)]
    pub client_key_id: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub upstream_model: Option<String>,
    #[serde(default)]
    pub requested_model: Option<String>,
    pub completed_at_ms: u64,
    #[serde(default)]
    pub terminal: Option<Terminal>,
    #[serde(default)]
    pub failure: Option<Failure>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sdk_observation_shape() {
        let value = serde_json::json!({
            "event_id": "obs_1",
            "request_id": "req_1",
            "config_revision": 7,
            "operation": "responses",
            "client_key_id": "key_1",
            "account_id": "acct_1",
            "upstream_model": "gpt-5.6-sol",
            "requested_model": "gpt-5.6-sol-excel",
            "provider": "openai",
            "completed_at_ms": 1730000000000u64,
            "terminal": {"outcome": "succeeded", "send_state": "upstream_responded", "attempt_count": 1},
            "usage": {"input_tokens": 22357, "output_tokens": 7, "total_tokens": 22364}
        });
        let event: ObserveEvent = serde_json::from_value(value).unwrap();
        assert_eq!(event.client_key_id.as_deref(), Some("key_1"));
        assert_eq!(event.usage.as_ref().unwrap().input_tokens, Some(22357));
        assert_eq!(event.terminal.as_ref().unwrap().outcome, Outcome::Succeeded);
    }
}
