//! attempt 中间件只处理 CPR 原生绑定已匹配的请求。
//! Key / 账号组 / 模型范围由宿主检查；插件不读取客户端密钥，
//! 不把静态 isolationScope 或完成后的观察记录当作请求 Key 身份。
mod dial;
mod management;
mod observation;
use cpr_excel_companion::auth::{Context, sign};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    client::{PluginBuilder, PluginSession, RequestCall, SessionConfig},
};
use serde_json::Value;
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

const EXCEL_SUFFIX: &str = "-excel";

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
        "excelModelSuffix": EXCEL_SUFFIX,
        "routingMode": "host_binding",
    }));
    let plugin = PluginBuilder::from_json(include_bytes!("../plugin.json"))?
        .middleware(move |mut call: RequestCall| {
            let secret = secret.clone();
            let scope = scope.clone();
            async move {
                let account = call.request.head.account_id.clone().ok_or_else(|| {
                    PluginFault::new(ErrorCode::InvalidInput, "attempt has no selected account")
                })?;
                // 连接池会复用握手头；逐条消息必须带本次 attempt 的签名上下文。
                let mut body: Value = serde_json::from_slice(&call.request.body).map_err(|_| {
                    PluginFault::new(ErrorCode::InvalidInput, "bridge requires a JSON body")
                })?;
                let object = body.as_object_mut().ok_or_else(|| {
                    PluginFault::new(ErrorCode::InvalidInput, "bridge requires an object body")
                })?;
                // 后缀表达请求偏好；桥接再应用已绑定范围共用的模型通道。
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
                        key: None,
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
        .on(gateway_plugin_sdk::client::methods::OBSERVE, move |call| {
            let secret = observe_secret.clone();
            let url = observe_url.clone();
            async move { observation::forward(call, &url, &secret).await }
        })?
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
    fn bridge_adapter_keeps_attempt_stage() {
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
