//! attempt 中间件签发桥接上下文；`-excel` 模型经 base_url 指向的桥接转发，
//! 数据面走 CPR 原生 Provider（计费完整），控制面由插件进程直连桥接。
mod dial;
mod management;
mod observation;
use cpr_excel_companion::auth::{Context, sign};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    client::{PluginBuilder, PluginSession, SessionConfig},
};
use serde_json::Value;
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
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
    let enabled = config["excelEnabled"].as_bool().unwrap_or(true);
    let suffix = config["excelModelSuffix"]
        .as_str()
        .unwrap_or("-excel")
        .to_owned();
    let control_secret = secret.clone();
    let control_url = config["bridgeControlUrl"]
        .as_str()
        .unwrap_or("http://127.0.0.1:8089/_control")
        .to_owned();
    let observe_secret = control_secret.clone();
    let observe_url = control_url.clone();
    let show_page = config["showPage"].as_bool().unwrap_or(true);
    let plugin = PluginBuilder::from_json(include_bytes!("../plugin.json"))?
        .middleware(move |mut call| {
            let secret = secret.clone();
            let scope = scope.clone();
            let suffix = suffix.clone();
            async move {
                if call.request.head.provider.as_deref() != Some("openai") {
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
                // 只判定不改写：宿主会按请求元数据覆写 body 的 model 字段，
                // 后缀由桥接的 prepare 还原为上游模型名。
                let requested = object
                    .get("model")
                    .and_then(Value::as_str)
                    .or(call.request.head.model.as_deref())
                    .unwrap_or("");
                let excel = enabled
                    && requested.ends_with(suffix.as_str())
                    && requested.len() > suffix.len();
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
            async move { management::handle(call, &url, &secret).await }
        })?
        .build()?;
    session.run(plugin).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn excel_suffix_detection_matches_semantics() {
        let suffix = "-excel";
        let detect = |model: &str| model.ends_with(suffix) && model.len() > suffix.len();
        assert!(detect("gpt-5.6-sol-excel"));
        assert!(!detect("gpt-5.6-sol"));
        assert!(!detect("-excel"));
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
