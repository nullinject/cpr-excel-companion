//! 管理与 CLI 的跨进程数据；参数和命令结果只通过有界二进制载荷传输。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::host::AuthSaveRequest;

/// `management.register` 冻结的路由与页面；路径均相对插件实例命名空间。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagementRegistration {
    #[serde(default)]
    pub routes: Vec<ManagementRoute>,
    #[serde(default)]
    pub resources: Vec<ManagementResource>,
    #[serde(default)]
    pub pages: Vec<ManagementPage>,
    #[serde(default)]
    pub callbacks: Vec<ManagementCallback>,
}

/// 仅接受宿主签发的一次性 state；回调不能访问账号、网络或私有状态宿主端口。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagementCallback {
    pub path: String,
    pub response_content_types: Vec<String>,
}

/// 管理 handler 默认且始终要求管理员身份；浏览器 Cookie、Authorization 不会传入插件。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagementRoute {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub request_content_types: Vec<String>,
    pub response_content_types: Vec<String>,
}

/// 资源必须已在包清单 resources 中声明并校验摘要，公开资源还须单独获得授权。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagementResource {
    pub path: String,
    #[serde(default)]
    pub public: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagementPage {
    pub id: String,
    pub title: String,
    /// 页面目录直接提供副标题，避免加载静态资源后再替换宿主标题区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub entry: String,
    pub icon: Option<String>,
}

/// `management.handle` 元数据；原始请求体独立放在帧 payload，不进行 JSON 二次编码。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagementRequest {
    pub method: String,
    pub path: String,
    pub query: String,
    pub content_type: Option<String>,
}

/// 原始响应体独立放在帧 payload；宿主拥有 CSP、安全头及大小限制。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagementResponse {
    pub status: u16,
    pub content_type: String,
}

/// `command_line.register` 的只读结果；注册和帮助查询不能执行命令或登录。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRegistration {
    pub commands: Vec<CommandDescriptor>,
}

/// 命令名只在插件实例的命名空间内生效，不注册宿主全局启动参数。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandDescriptor {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub parameters: Vec<CommandParameter>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandParameter {
    pub name: String,
    pub description: String,
    pub value_type: CommandParameterType,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub sensitive: bool,
    pub default: Option<CommandValue>,
}

/// `int` 固定为 32 位，duration 固定为有符号纳秒，避免随部署平台改变合同。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandParameterType {
    Bool,
    String,
    Int,
    Int64,
    Float64,
    Duration,
}

/// 参数可能含登录材料，故意不实现内容型 Debug。
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum CommandValue {
    Bool(bool),
    String(String),
    Int(i32),
    Int64(i64),
    Float64(f64),
    Duration(i64),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInvocation {
    pub name: String,
    pub arguments: BTreeMap<String, CommandValue>,
}

/// 输出由宿主原样交付 CLI 调用方，不进入普通诊断。
/// 待保存账号仅在退出码为零时按顺序经 Admin 提交；每项独立 CAS，失败不自动重试。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: u8,
    #[serde(default)]
    pub accounts: Vec<AuthSaveRequest>,
}
