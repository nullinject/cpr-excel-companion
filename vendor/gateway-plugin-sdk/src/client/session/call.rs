//! 插件业务调用、处理器与响应合同

use std::{future::Future, pin::Pin};

use serde_json::Value;

use crate::{CallContext, PluginFault};

use super::{CallCancellation, HostClient, ResponseStream};

/// 交给插件业务处理器的一次宿主调用；不实现 `Debug`，避免敏感载荷泄露
pub struct PluginCall {
    pub method: String,
    pub context: CallContext,
    pub params: Value,
    pub payload: Vec<u8>,
    pub host: HostClient,
    pub cancellation: CallCancellation,
}

/// 插件业务调用 future
pub type CallFuture<'a> = Pin<Box<dyn Future<Output = Result<CallReply, PluginFault>> + Send + 'a>>;

/// 插件只实现业务调用；会话负责握手、关联、流控、取消和关闭
pub trait PluginHandler: Send + Sync + 'static {
    fn call(&self, call: PluginCall) -> CallFuture<'_>;

    /// 取消时释放只属于该调用的临时业务状态
    /// 实现必须幂等、快速且不得执行阻塞 I/O
    fn cancel(&self, _context: &CallContext) {}

    /// 会话停止接收新调用时触发
    /// 实现必须幂等、快速且不得执行阻塞 I/O
    fn quiesce(&self) {}

    /// 会话关闭时释放插件自有临时状态
    /// 实现必须幂等、快速且不得执行阻塞 I/O
    fn shutdown(&self) {}
}

/// 一次业务成功结果；`stream` 存在时 SDK 自动产生单一流终态
pub struct CallReply {
    pub(super) result: Value,
    pub(super) payload: Vec<u8>,
    pub(super) stream: Option<ResponseStream>,
}

impl CallReply {
    #[must_use]
    pub fn unary(result: Value, payload: Vec<u8>) -> Self {
        Self {
            result,
            payload,
            stream: None,
        }
    }

    #[must_use]
    pub fn stream(result: Value, payload: Vec<u8>, stream: ResponseStream) -> Self {
        Self {
            result,
            payload,
            stream: Some(stream),
        }
    }
}
