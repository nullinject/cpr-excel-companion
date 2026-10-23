//! 插件消息帧的异步读写与元数据、载荷长度校验

use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use crate::{Frame, FrameError, Message};
use serde::{Deserialize, Serialize};

const MAX_METADATA_BYTES: usize = 64 * 1024;
const IO_CHUNK_BYTES: usize = 256 * 1024;

#[derive(Serialize)]
struct EncodedMetadata<'a> {
    #[serde(flatten)]
    message: &'a Message,
    #[serde(skip_serializing_if = "is_zero")]
    request_context_bytes: usize,
    #[serde(skip_serializing_if = "is_zero")]
    data_bytes: usize,
}

#[derive(Deserialize)]
struct DecodedMetadata {
    #[serde(flatten)]
    message: Message,
    #[serde(default)]
    request_context_bytes: usize,
    #[serde(default)]
    data_bytes: usize,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// 入队前完成编码；写泵复用已验证的字节，不重复序列化入口快照
pub struct EncodedFrame {
    frame: Frame,
    sections: EncodedSections,
}

struct EncodedSections {
    metadata: Vec<u8>,
    request: Vec<u8>,
    data: Vec<u8>,
}

impl EncodedFrame {
    pub fn new(mut frame: Frame) -> Result<Self, FrameError> {
        let sections = encode_sections(&frame)?;
        // 排队只持有编码后的业务数据，避免大型 JSON 与其编码副本同时驻留
        if let Some(value) = frame.message.data_mut() {
            *value = serde_json::Value::Null;
        }
        if let Message::Call { context, .. } = &mut frame.message {
            context.request = None;
        }
        Ok(Self { frame, sections })
    }

    /// 仅供写泵关联消息类别和 ID；业务 JSON 与入口快照已移入编码字节
    #[must_use]
    pub fn message(&self) -> &Message {
        &self.frame.message
    }

    pub async fn write<W: AsyncWrite + Unpin>(&self, writer: &mut W) -> Result<(), FrameError> {
        write_encoded(
            writer,
            &self.sections.metadata,
            &self.sections.request,
            &self.sections.data,
            &self.frame.payload,
        )
        .await
    }
}

/// 读取完整消息；I/O 分块是传输细节，不限制业务正文总量
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Frame, FrameError> {
    let metadata_len = reader.read_u32().await? as usize;
    let payload_len = usize::try_from(reader.read_u64().await?).map_err(|_| FrameError::Length)?;
    check_metadata_length(metadata_len)?;
    let mut metadata = vec![0; metadata_len];
    reader.read_exact(&mut metadata).await?;
    let DecodedMetadata {
        mut message,
        request_context_bytes,
        data_bytes,
    } = serde_json::from_slice(&metadata).map_err(FrameError::Metadata)?;
    let body_bytes = payload_len
        .checked_sub(request_context_bytes)
        .and_then(|bytes| bytes.checked_sub(data_bytes))
        .ok_or(FrameError::Length)?;
    if message.data().is_some() != (data_bytes > 0) {
        return Err(FrameError::Length);
    }
    if request_context_bytes > 0 {
        let Message::Call { context, .. } = &mut message else {
            return Err(FrameError::Length);
        };
        let request = read_payload(reader, request_context_bytes).await?;
        context.request = Some(std::sync::Arc::new(
            serde_json::from_slice(&request).map_err(FrameError::Metadata)?,
        ));
    }
    if let Some(value) = message.data_mut() {
        let data = read_payload(reader, data_bytes).await?;
        *value = serde_json::from_slice(&data).map_err(FrameError::Metadata)?;
    }
    let payload = read_payload(reader, body_bytes).await?;
    Ok(Frame { message, payload })
}

async fn read_payload<R: AsyncRead + Unpin>(
    reader: &mut R,
    payload_len: usize,
) -> Result<Vec<u8>, FrameError> {
    let mut payload = Vec::new();
    // 按实际读取进度分配，避免仅凭对端声明的大长度立即占满内存
    while payload.len() < payload_len {
        let amount = (payload_len - payload.len()).min(IO_CHUNK_BYTES);
        payload
            .try_reserve(amount)
            .map_err(|_| FrameError::Io(std::io::Error::from(std::io::ErrorKind::OutOfMemory)))?;
        let start = payload.len();
        payload.resize(start + amount, 0);
        reader.read_exact(&mut payload[start..]).await?;
    }
    Ok(payload)
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &Frame,
) -> Result<(), FrameError> {
    let sections = encode_sections(frame)?;
    write_encoded(
        writer,
        &sections.metadata,
        &sections.request,
        &sections.data,
        &frame.payload,
    )
    .await
}

async fn write_encoded<W: AsyncWrite + Unpin>(
    writer: &mut W,
    metadata: &[u8],
    request: &[u8],
    data: &[u8],
    payload: &[u8],
) -> Result<(), FrameError> {
    let metadata_len = u32::try_from(metadata.len()).map_err(|_| FrameError::Length)?;
    let payload_len = payload
        .len()
        .checked_add(request.len())
        .and_then(|bytes| bytes.checked_add(data.len()))
        .ok_or(FrameError::Length)?;
    let payload_len = u64::try_from(payload_len).map_err(|_| FrameError::Length)?;
    writer.write_u32(metadata_len).await?;
    writer.write_u64(payload_len).await?;
    writer.write_all(metadata).await?;
    // 各段分别写入，避免为 JSON 前缀复制或拼接二进制正文
    for chunk in request
        .chunks(IO_CHUNK_BYTES)
        .chain(data.chunks(IO_CHUNK_BYTES))
        .chain(payload.chunks(IO_CHUNK_BYTES))
    {
        writer.write_all(chunk).await?;
    }
    writer.flush().await?;
    Ok(())
}

/// 入队前校验本地元数据，避免单次编码失败中断共享传输
pub fn validate_frame(frame: &Frame) -> Result<(), FrameError> {
    encode_sections(frame).map(drop)
}

fn encode_sections(frame: &Frame) -> Result<EncodedSections, FrameError> {
    let request = match &frame.message {
        Message::Call { context, .. } => context.request.as_deref(),
        _ => None,
    }
    .map(encode_json)
    .transpose()?
    .unwrap_or_default();
    let data = frame
        .message
        .data()
        .map(encode_json)
        .transpose()?
        .unwrap_or_default();
    let metadata = encode_json(&EncodedMetadata {
        message: &frame.message,
        request_context_bytes: request.len(),
        data_bytes: data.len(),
    })?;
    check_metadata_length(metadata.len())?;
    Ok(EncodedSections {
        metadata,
        request,
        data,
    })
}

fn encode_json(value: &impl Serialize) -> Result<Vec<u8>, FrameError> {
    struct Buffer(Vec<u8>);

    impl std::io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .try_reserve(bytes.len())
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::OutOfMemory))?;
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut buffer = Buffer(Vec::new());
    serde_json::to_writer(&mut buffer, value).map_err(FrameError::Metadata)?;
    Ok(buffer.0)
}

fn check_metadata_length(metadata: usize) -> Result<(), FrameError> {
    if metadata == 0 || metadata > MAX_METADATA_BYTES {
        return Err(FrameError::Length);
    }
    Ok(())
}
