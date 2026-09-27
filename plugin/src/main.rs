//! 仅通过官方 attempt 中间件添加桥接上下文；next 保留原生 Provider 元数据。
mod management;
use cpr_excel_companion::auth::{Context, sign};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    client::{PluginBuilder, PluginSession, SessionConfig},
};
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
    let enabled = config["excelEnabled"].as_bool().unwrap_or(false);
    let control_secret = secret.clone();
    let control_url = config["bridgeControlUrl"]
        .as_str()
        .unwrap_or("https://example.invalid/excel-companion/_control")
        .to_owned();
    let show_page = config["showPage"].as_bool().unwrap_or(true);
    let plugin = PluginBuilder::from_json(include_bytes!("../plugin.json"))?
        .middleware(move |mut call| {
            let secret = secret.clone();
            let scope = scope.clone();
            async move {
                if call.request.head.provider.as_deref() != Some("openai") {
                    return call.next.run(call.request).await;
                }
                let account = call.request.head.account_id.clone().ok_or_else(|| {
                    PluginFault::new(ErrorCode::InvalidInput, "attempt has no selected account")
                })?;
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let token = sign(
                    &Context {
                        account,
                        scope,
                        request_id: call.request.head.request_id.clone(),
                        excel: enabled,
                        expires: now + 900,
                    },
                    &secret,
                )
                .map_err(|e| PluginFault::new(ErrorCode::InvalidInput, e))?;
                call.request.remove_header("x-excel-bridge-context");
                call.request
                    .append_header("x-excel-bridge-context", token.clone().into_bytes());
                // 连接池会复用握手头；逐条消息必须带本次 attempt 的签名上下文。
                let mut body: serde_json::Value = serde_json::from_slice(&call.request.body)
                    .map_err(|_| {
                        PluginFault::new(ErrorCode::InvalidInput, "bridge requires a JSON body")
                    })?;
                let object = body.as_object_mut().ok_or_else(|| {
                    PluginFault::new(ErrorCode::InvalidInput, "bridge requires an object body")
                })?;
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
