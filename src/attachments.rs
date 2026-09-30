//! 使用同一账户的出站客户端上传附件；失败时拒绝请求，不静默删除图片或文件。
use crate::{BridgeError as CodexClientError, BridgeResult as CodexClientResult};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::{
    Client,
    header::{HeaderMap, HeaderValue},
};
use serde_json::{Value, json};

const MAX_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;
const ATTACHMENTS_URL: &str = "https://bps.openai.com/basispoints/api/attachments";

fn decode(raw: &str) -> CodexClientResult<Vec<u8>> {
    let encoded = if raw.starts_with("data:") {
        let (prefix, content) = raw.split_once(',').ok_or(CodexClientError::ExcelRequest(
            "invalid attachment data URL",
        ))?;
        if !prefix.ends_with(";base64") {
            return Err(CodexClientError::ExcelRequest(
                "attachment must use base64 encoding",
            ));
        }
        content
    } else {
        raw
    };
    if encoded.len() > MAX_ATTACHMENT_BYTES.div_ceil(3) * 4 {
        return Err(CodexClientError::ExcelRequest("attachment exceeds 20 MiB"));
    }
    let data = STANDARD
        .decode(encoded)
        .map_err(|_| CodexClientError::ExcelRequest("invalid attachment base64"))?;
    if data.is_empty() || data.len() > MAX_ATTACHMENT_BYTES {
        return Err(CodexClientError::ExcelRequest(
            "attachment is empty or exceeds 20 MiB",
        ));
    }
    Ok(data)
}

pub async fn upload_inputs(
    client: &Client,
    headers: &HeaderMap,
    source: &mut Value,
) -> CodexClientResult<()> {
    upload_with(source, |content_type, body| async move {
        let mut upload_headers = headers.clone();
        upload_headers.remove("accept");
        upload_headers.insert("content-type", HeaderValue::from_str(&content_type)?);
        let mut response = client.post(ATTACHMENTS_URL).headers(upload_headers).body(body)
            .timeout(std::time::Duration::from_secs(120)).send().await?;
        if !response.status().is_success() { return Err(CodexClientError::ExcelRequest("Excel attachment upload rejected")); }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len() + chunk.len() > 65536 { return Err(CodexClientError::ExcelRequest("attachment response exceeds limit")); }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }).await
}

pub async fn upload_with<F, Fut>(source: &mut Value, mut upload: F) -> CodexClientResult<()>
where F: FnMut(String, Vec<u8>) -> Fut,
      Fut: std::future::Future<Output = CodexClientResult<Vec<u8>>>,
{
    let Some(input) = source.get_mut("input").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for item in input {
        let Some(content) = item.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for part in content {
            let kind = part.get("type").and_then(Value::as_str).unwrap_or("");
            if !matches!(kind, "input_image" | "input_file") {
                continue;
            }
            if part
                .get("file_id")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty())
            {
                continue;
            }
            let image = kind == "input_image";
            let raw = part
                .get(if image { "image_url" } else { "file_data" })
                .and_then(Value::as_str)
                .ok_or(CodexClientError::ExcelRequest(
                    "attachment requires inline data or file_id",
                ))?;
            if image && !raw.starts_with("data:") {
                return Err(CodexClientError::ExcelRequest(
                    "remote image URLs are unsupported; use inline data or file_id",
                ));
            }
            let data = decode(raw)?;
            let media_type = raw
                .strip_prefix("data:")
                .and_then(|s| s.split_once(';'))
                .map(|p| p.0)
                .unwrap_or("application/octet-stream");
            let (media_type, default_name) = match media_type {
                "image/png" => ("image/png", "image.png"),
                "image/jpeg" => ("image/jpeg", "image.jpg"),
                "image/webp" => ("image/webp", "image.webp"),
                "image/gif" => ("image/gif", "image.gif"),
                "application/pdf" => ("application/pdf", "document.pdf"),
                "text/plain" => ("text/plain", "document.txt"),
                _ if image => {
                    return Err(CodexClientError::ExcelRequest(
                        "unsupported image media type",
                    ));
                }
                _ => ("application/octet-stream", "upload.bin"),
            };
            let filename = part
                .get("filename")
                .and_then(Value::as_str)
                .filter(|name| {
                    !name.is_empty()
                        && name.len() <= 128
                        && name
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                })
                .unwrap_or(default_name);
            let boundary = format!("cpr-excel-{}", uuid::Uuid::new_v4().simple());
            let mut body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: {media_type}\r\n\r\n").into_bytes();
            body.extend_from_slice(&data);
            body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
            let bytes = upload(format!("multipart/form-data; boundary={boundary}"), body).await?;
            let uploaded: Value = serde_json::from_slice(&bytes).map_err(|_| {
                CodexClientError::ExcelRequest("invalid attachment upload response")
            })?;
            let id = uploaded
                .get("openai_file_id")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .ok_or(CodexClientError::ExcelRequest(
                    "attachment upload returned no file ID",
                ))?;
            let detail = part.get("detail").cloned().unwrap_or(json!("auto"));
            *part = if image {
                json!({"type":"input_image", "file_id":id, "detail":detail})
            } else {
                json!({"type":"input_file", "file_id":id})
            };
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_and_empty_attachment_data() {
        assert!(decode("data:image/png,abc").is_err());
        assert!(decode("@@@").is_err());
        assert!(decode("").is_err());
        assert_eq!(decode("data:text/plain;base64,aGVsbG8=").unwrap(), b"hello");
    }
}
