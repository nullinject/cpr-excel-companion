//! 插件响应流的有界生产、拉取与分块校验

use std::{collections::VecDeque, future::Future, num::NonZeroUsize, pin::Pin};

use tokio::sync::mpsc;

use crate::{ErrorCode, PluginFault};

use super::SessionError;

enum StreamSource {
    Buffered(VecDeque<Vec<u8>>),
    Channel(mpsc::Receiver<Result<Vec<u8>, PluginFault>>),
    Pull(Box<dyn PullResponseStream>),
}

pub type PullResponseFuture<'a> =
    Pin<Box<dyn Future<Output = Option<Result<Vec<u8>, PluginFault>>> + Send + 'a>>;

pub trait PullResponseStream: Send {
    fn next(&mut self) -> PullResponseFuture<'_>;
}

/// SDK 管理 sequence、Credit 和终态的响应流
pub struct ResponseStream {
    source: StreamSource,
    declared_capacity: usize,
}

impl ResponseStream {
    /// 从已完成业务校验的有限分块构造流；SDK 会在发送 `Result` 前预检全部分块
    #[must_use]
    pub fn from_chunks(chunks: Vec<Vec<u8>>) -> Self {
        let declared_capacity = chunks.len().max(1);
        Self {
            source: StreamSource::Buffered(chunks.into()),
            declared_capacity,
        }
    }

    /// 创建由生产者驱动的有界流队列
    #[must_use]
    pub fn channel(capacity: NonZeroUsize) -> (StreamSender, Self) {
        let (sender, receiver) = mpsc::channel(capacity.get());
        (
            StreamSender { sender },
            Self {
                source: StreamSource::Channel(receiver),
                declared_capacity: capacity.get(),
            },
        )
    }

    /// 按消费进度拉取下一帧；返回 None 表示完成，丢弃流会丢弃生产者
    pub fn pull(source: Box<dyn PullResponseStream>) -> Self {
        Self {
            source: StreamSource::Pull(source),
            declared_capacity: 1,
        }
    }

    pub(super) fn validate_buffered(
        &self,
        maximum_chunks: usize,
        maximum_stream_chunk_bytes: usize,
        window_bytes: u64,
    ) -> Result<(), PluginFault> {
        if self.declared_capacity > maximum_chunks {
            return Err(capacity_fault(
                "response stream exceeds the local queue limit",
            ));
        }
        if let StreamSource::Buffered(chunks) = &self.source {
            for chunk in chunks {
                validate_stream_chunk(chunk, maximum_stream_chunk_bytes, window_bytes)?;
            }
        }
        Ok(())
    }

    pub(crate) async fn next(&mut self) -> Option<Result<Vec<u8>, PluginFault>> {
        match &mut self.source {
            StreamSource::Buffered(chunks) => chunks.pop_front().map(Ok),
            StreamSource::Channel(receiver) => receiver.recv().await,
            StreamSource::Pull(source) => source.next().await,
        }
    }
}

/// 动态响应流的有界生产端；最后一个 sender 被释放表示成功终态
#[derive(Clone)]
pub struct StreamSender {
    sender: mpsc::Sender<Result<Vec<u8>, PluginFault>>,
}

impl StreamSender {
    /// 排队一个业务分块；实际线协议流控由会话完成
    pub async fn send(&self, payload: Vec<u8>) -> Result<(), SessionError> {
        self.sender
            .send(Ok(payload))
            .await
            .map_err(|_| SessionError::Closed)
    }

    /// 以业务错误结束动态流
    pub async fn fail(&self, fault: PluginFault) -> Result<(), SessionError> {
        self.sender
            .send(Err(fault))
            .await
            .map_err(|_| SessionError::Closed)
    }
}

pub(super) fn validate_stream_chunk(
    payload: &[u8],
    maximum_stream_chunk_bytes: usize,
    window_bytes: u64,
) -> Result<(), PluginFault> {
    if payload.is_empty() {
        return Err(PluginFault::new(
            ErrorCode::InvalidInput,
            "stream chunks must not be empty",
        ));
    }
    if payload.len() as u64 > window_bytes {
        return Err(PluginFault::new(
            ErrorCode::InvalidInput,
            "response event exceeds the host stream credit window",
        ));
    }
    if payload.len() > maximum_stream_chunk_bytes {
        return Err(PluginFault::new(
            ErrorCode::InvalidInput,
            "response event exceeds the stream chunk budget",
        ));
    }
    Ok(())
}

fn capacity_fault(message: &'static str) -> PluginFault {
    PluginFault::new(ErrorCode::Capacity, message)
}
