//! 只读响应观察调用的数据合同。

use serde::{Deserialize, Serialize};

/// 实际上游 WebSocket 响应事件。
///
/// 原始 JSON 在 RPC 二进制载荷中独立传递。载荷未纳入本次有界观察时
/// `payload_included=false`、载荷为空，且不会通过事件名或头字段旁路泄漏正文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObserveWebSocketResponse {
    pub event_id: String,
    pub request_id: String,
    pub config_revision: u64,
    pub operation: String,
    pub protocol: String,
    pub provider: String,
    pub attempt_index: u32,
    /// 请求内从 1 开始单调递增，切换 attempt 时不重置；有界投递可能产生间隔。
    pub sequence: u64,
    pub payload_included: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    /// 实际 attempt 选择的账号；属于 requests 域的请求处理事实，不包含账号资料或凭据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// 事件名属于原始响应内容，只在 `payload_included=true` 时提供。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
}
