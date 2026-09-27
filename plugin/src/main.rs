//! 双阶段中间件：
//! request 阶段读取客户端 API key 摘要并暂存（宿主仅在 attempt 阶段隐藏身份）；
//! attempt 阶段签发带 key 摘要的桥接上下文。`-excel` 后缀表达客户端的通道偏好，
//! 按 Key × 模型的通道覆盖由桥接策略决定。插件配置保持最小。
mod dial;
mod management;
mod observation;
use cpr_excel_companion::auth::{Context, sign};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::middleware::MiddlewareMount,
    client::{PluginBuilder, PluginSession, SessionConfig},
};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const EXCEL_SUFFIX: &str = "-excel";

/// request 阶段暂存：request_id → (key 摘要, 写入时间)；attempt 阶段取用。
/// 有界：超量时丢弃过期条目，TTL 15 分钟与签名有效期一致。
fn stash() -> &'static Mutex<HashMap<String, (String, Instant)>> {
    static STASH: OnceLock<Mutex<HashMap<String, (String, Instant)>>> = OnceLock::new();
    STASH.get_or_init(|| Mutex::new(HashMap::new()))
}
fn stash_insert(request_id: &str, key_hash: String) {
    let mut map = stash().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if map.len() > 20_000 {
        map.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(900));
        if map.len() > 20_000 {
            map.clear();
        }
    }
    map.insert(request_id.to_owned(), (key_hash, Instant::now()));
}
fn stash_take(request_id: &str) -> Option<String> {
    let mut map = stash().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    map.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(900));
    map.get(request_id).map(|(hash, _)| hash.clone())
}

fn key_hash_from_headers(
    headers: &[gateway_plugin_sdk::call::middleware::MiddlewareHeader],
) -> Option<String> {
    use sha2::{Digest, Sha256};
    let raw = &headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("authorization"))?
        .value;
    let text = std::str::from_utf8(raw).ok()?;
    let key = text.strip_prefix("Bearer ").unwrap_or(text).trim();
    if key.is_empty() {
        return None;
    }
    Some(hex::encode(Sha256::digest(key.as_bytes())))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let session = PluginSession::accept(
        tokio::io::stdin(),
        tokio::io::stdout(),
        SessionConfig::default(),
    )
    .await?;
    let config = &session.handshake().configuration;
    let secret = Arc::new(std::fs::read(
        config["secretFile"]
            .as_str()
            .unwrap_or("/run/secrets/excel-bridge.key"),
    )?);
    if secret.len() < 32 {
        return Err("bridge secret must contain at least 32 bytes".into());
    }
    let scope = config["isolationScope"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("isolationScope is required")?
        .to_owned();
    let control_secret = secret.clone();
    let control_url = config["bridgeControlUrl"]
        .as_str()
        .unwrap_or("http://127.0.0.1:8089/_control")
        .to_owned();
    let observe_secret = control_secret.clone();
    let observe_url = control_url.clone();
    let show_page = config["showPage"].as_bool().unwrap_or(true);
    let plugin_info = Arc::new(serde_json::json!({
        "isolationScope": scope,
    }));
    let plugin = PluginBuilder::from_json(include_bytes!("../plugin.json"))?
        .middleware(move |mut call| {
            let secret = secret.clone();
            let scope = scope.clone();
            async move {
                // request 阶段：提取 key 摘要并原样放行。宿主只在 attempt 阶段
                // 提供数据，这里是把 per-key 决策传入桥接的唯一通道。
                if call.request.head.mount == MiddlewareMount::Request {
                    // CPR 3.16.0 的 request 头投影只含 user-agent/content-type，
                    // 无客户端身份；若未来版本投影 authorization，key 规则自动生效。
                    if let Some(hash) = key_hash_from_headers(&call.request.head.headers) {
                        stash_insert(&call.request.head.request_id, hash);
                    }
                    return call.next.run(call.request).await;
                }
                let account = call.request.head.account_id.clone().ok_or_else(|| {
                    PluginFault::new(ErrorCode::InvalidInput, "attempt has no selected account")
                })?;
                // 连接池会复用握手头；逐条消息必须带本次 attempt 的签名上下文。
                let mut body: Value = serde_json::from_slice(&call.request.body)
                    .map_err(|_| {
                        PluginFault::new(ErrorCode::InvalidInput, "bridge requires a JSON body")
                    })?;
                let object = body.as_object_mut().ok_or_else(|| {
                    PluginFault::new(ErrorCode::InvalidInput, "bridge requires an object body")
                })?;
                // 后缀只表达客户端的通道偏好；按 Key 的通道覆盖在桥接策略里。
                let requested = object
                    .get("model")
                    .and_then(Value::as_str)
                    .or(call.request.head.model.as_deref())
                    .unwrap_or("");
                let excel =
                    requested.ends_with(EXCEL_SUFFIX) && requested.len() > EXCEL_SUFFIX.len();
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let token = sign(
                    &Context {
                        account,
                        scope,
                        request_id: call.request.head.request_id.clone(),
                        excel,
                        key: Some(
                            stash_take(&call.request.head.request_id)
                                .unwrap_or_else(|| "no-stash".into()),
                        ),
                        expires: now + 900,
                    },
                    &secret,
                )
                .map_err(|e| PluginFault::new(ErrorCode::InvalidInput, e))?;
                call.request.remove_header("x-excel-bridge-context");
                call.request
                    .append_header("x-excel-bridge-context", token.clone().into_bytes());
                let metadata = object
                    .entry("client_metadata")
                    .or_insert_with(|| serde_json::json!({}));
                let metadata = metadata.as_object_mut().ok_or_else(|| {
                    PluginFault::new(ErrorCode::InvalidInput, "client_metadata must be an object")
                })?;
                metadata.insert("_cpr_excel_bridge".into(), serde_json::Value::String(token));
                call.request
                    .replace_body(serde_json::to_vec(&body).map_err(|_| {
                        PluginFault::new(ErrorCode::InvalidInput, "invalid bridge body")
                    })?);
                call.next.run(call.request).await
            }
        })?
        .on(
            gateway_plugin_sdk::client::methods::OBSERVE_REQUEST,
            move |call| {
                let secret = observe_secret.clone();
                let url = observe_url.clone();
                async move { observation::forward(call, &url, &secret).await }
            },
        )?
        .management(management::registration(show_page), move |call| {
            let secret = control_secret.clone();
            let url = control_url.clone();
            let info = plugin_info.clone();
            async move { management::handle(call, &url, &secret, &info).await }
        })?
        .build()?;
    session.run(plugin).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::EXCEL_SUFFIX;

    #[test]
    fn excel_suffix_detection_matches_semantics() {
        let suffix = EXCEL_SUFFIX;
        let detect = |model: &str| model.ends_with(suffix) && model.len() > suffix.len();
        assert!(detect("gpt-5.6-sol-excel"));
        assert!(!detect("gpt-5.6-sol"));
        assert!(!detect("-excel"));
    }

    #[test]
    fn official_316_manifest_avoids_request_websocket_framing_bug() {
        let value: serde_json::Value =
            serde_json::from_slice(include_bytes!("../plugin.json")).unwrap();
        assert_eq!(
            value["contributes"]["middleware"]["stages"],
            serde_json::json!(["attempt"])
        );
    }

    #[test]
    fn author_manifest_is_accepted_by_pinned_official_sdk() {
        let manifest =
            gateway_plugin_sdk::Manifest::from_author_slice(include_bytes!("../plugin.json"))
                .unwrap();
        assert_eq!(
            format!("{}.{}", manifest.publisher, manifest.name),
            "nullinject.excel-companion"
        );
    }
}
