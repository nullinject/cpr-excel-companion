//! CPR owns account selection, credentials, proxies and billing.
mod adapter;
mod auth;
mod management;
mod observation;
mod wire;
use cpr_excel_companion::control::Control;
use gateway_plugin_sdk::client::{
    PluginBuilder, PluginSession, SessionConfig, TypedReply, methods,
};
use std::{path::PathBuf, sync::Arc};
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
    let path = PathBuf::from(
        config["policyFile"]
            .as_str()
            .ok_or("policyFile is required")?,
    );
    if !path.is_absolute() {
        return Err("policyFile must be absolute".into());
    }
    let control = Arc::new(Control::load(path.clone())?);
    // 账户级 Excel 授权与会话状态；缺省文件不存在时视为全部未授权。
    let auth = Arc::new(auth::Auth::load(
        path.parent()
            .ok_or("policyFile needs a parent directory")?
            .join("account-auth.json"),
    )?);
    let execute_control = control.clone();
    let execute_auth = auth.clone();
    let observe_control = control.clone();
    let show_page = config["showPage"].as_bool().unwrap_or(true);
    let info = Arc::new(serde_json::json!({
        "isolationScope": session.handshake().instance_id,
        "excelModelSuffix": EXCEL_SUFFIX, "routingMode": "host_binding", "integration": "managed_upstream"
    }));
    let plugin = PluginBuilder::from_json(include_bytes!("../plugin.json"))?
        .on(methods::UPSTREAM_ADAPTER_REGISTER, |_| async {
            Ok(TypedReply::new(adapter::registration()))
        })?
        .on(methods::UPSTREAM_ADAPTER_EXECUTE, move |call| {
            let control = execute_control.clone();
            let auth = execute_auth.clone();
            async move { adapter::execute(call, control, auth).await }
        })?
        .on(methods::OBSERVE, move |call| {
            let control = observe_control.clone();
            async move { observation::handle(call, &control).await }
        })?
        .management(management::registration(show_page), move |call| {
            let control = control.clone();
            let auth = auth.clone();
            let info = info.clone();
            async move { management::handle(call, &control, &info, &auth).await }
        })?
        .build()?;
    session.run(plugin).await?;
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn author_manifest_is_accepted_by_pinned_official_sdk() {
        gateway_plugin_sdk::Manifest::from_author_slice(include_bytes!("../plugin.json")).unwrap();
    }
}
