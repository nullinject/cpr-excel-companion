//! 把宿主最终用量/失败观察转发给桥接，归并进请求记录（进程内直连控制通道）。
//! 观察投递有界且不重投；转发失败只影响插件侧记录，不影响宿主结算。
use crate::management::remote;
use gateway_plugin_sdk::{
    call::policy::ObserveRequest,
    client::{Empty, TypedCall, TypedReply},
    ErrorCode, PluginFault,
};

pub async fn forward(
    call: TypedCall<ObserveRequest>,
    url: &str,
    secret: &[u8],
) -> Result<TypedReply<Empty>, PluginFault> {
    let observation = call.request;
    let payload = serde_json::to_vec(&observation)
        .map_err(|_| PluginFault::new(ErrorCode::InvalidInput, "observation encoding failed"))?;
    // 观察回调不在请求路径上；转发失败只损失插件侧一条记录的终态合并。
    let _ = remote(&call.host, url, secret, &observation.event_id, "observe", payload).await;
    Ok(TypedReply::new(Empty {}))
}
