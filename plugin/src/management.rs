//! 页面只使用官方宿主桥读取 Key/账户；桥接控制调用走进程内直连和逐请求签名。
use crate::dial;
use cpr_excel_companion::auth::{Context, sign};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        host::{KeyListRequest, KeyListResult},
        management::{
            ManagementPage, ManagementRegistration, ManagementRequest, ManagementResource,
            ManagementResponse, ManagementRoute,
        },
    },
    client::{HostClient, SessionError, TypedCall, TypedReply},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
fn fault(message: &str) -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, message)
}
pub async fn remote(
    _host: &HostClient,
    url: &str,
    secret: &[u8],
    id: &str,
    operation: &str,
    payload: Vec<u8>,
) -> Result<(u16, Vec<u8>), PluginFault> {
    let mut hash = Sha256::new();
    hash.update(operation.as_bytes());
    hash.update(b"\n");
    hash.update(&payload);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let context = Context {
        account: "control".into(),
        scope: hex::encode(hash.finalize()),
        request_id: id.into(),
        excel: false,
        key: None,
        expires: now + 30,
    };
    let token = sign(&context, secret).map_err(fault)?;
    let reply = dial::post(
        &format!("{}/{operation}", url.trim_end_matches('/')),
        vec![
            ("authorization".into(), format!("Bridge {token}")),
            ("content-type".into(), "application/json".into()),
        ],
        payload,
    )
    .await
    .map_err(|message| fault(&message))?;
    Ok((reply.status, reply.body))
}
fn reply(status: u16, payload: Vec<u8>) -> TypedReply<ManagementResponse> {
    TypedReply::new(ManagementResponse {
        status,
        content_type: "application/json".into(),
        headers: vec![],
    })
    .with_payload(payload)
}
pub async fn handle(
    call: TypedCall<ManagementRequest>,
    url: &str,
    secret: &[u8],
    plugin_info: &serde_json::Value,
) -> Result<TypedReply<ManagementResponse>, PluginFault> {
    if !call.request.query.is_empty() || call.payload.len() > 65536 {
        return Ok(reply(400, br#"{"error":"invalid request"}"#.to_vec()));
    }
    if call.request.method == "GET" && call.request.path == "api/plugin-info" {
        return Ok(reply(
            200,
            serde_json::to_vec(plugin_info).unwrap_or_default(),
        ));
    }
    if call.request.method == "GET" && call.request.path == "api/options" {
        let response = call
            .host
            .call(
                "host.keys.list",
                serde_json::to_value(KeyListRequest {
                    cursor: None,
                    limit: 200,
                })
                .unwrap(),
                vec![],
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        let keys: KeyListResult =
            serde_json::from_value(response.result).map_err(|_| fault("invalid key list"))?;
        let response = call
            .host
            .call(
                "host.data.accounts.list",
                json!({}),
                serde_json::to_vec(&gateway_plugin_sdk::call::data::AccountFactsQuery {
                    provider_id: Some("openai".into()),
                    cursor: None,
                    limit: 200,
                })
                .unwrap(),
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        let accounts: gateway_plugin_sdk::call::data::AccountFactsPage =
            serde_json::from_slice(&response.payload).map_err(|_| fault("invalid account list"))?;
        return Ok(reply(200,serde_json::to_vec(&json!({"keys":keys.keys,"accounts":accounts.accounts,"more":keys.next_cursor.is_some()||accounts.next_cursor.is_some()})).unwrap()));
    }
    if call.request.method == "POST"
        && matches!(call.request.path.as_str(), "api/keys" | "api/accounts")
    {
        let cursor = match catalog_cursor(&call.payload) {
            Ok(cursor) => cursor,
            Err(message) => return Ok(json_error(400, message)),
        };
        let page = catalog_page(&call.host, &call.request.path, cursor).await?;
        return Ok(reply(
            200,
            serde_json::to_vec(&page).map_err(|_| fault("invalid catalog response"))?,
        ));
    }
    let (operation, payload) = match (call.request.method.as_str(), call.request.path.as_str()) {
        ("GET", "api/snapshot") if call.payload.is_empty() => ("snapshot", b"{}".to_vec()),
        ("POST", "api/policy") => ("policy", call.payload),
        ("POST", "api/observe") => ("observe", call.payload),
        _ => return Ok(reply(404, br#"{"error":"not found"}"#.to_vec())),
    };
    let result = remote(
        &call.host,
        url,
        secret,
        &call.context.resource_scope_id,
        operation,
        payload,
    )
    .await;
    Ok(match result {
        Ok((status, payload)) => reply(status, payload),
        Err(_) => json_error(502, "无法连接桥接服务，请检查服务状态与控制地址。"),
    })
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogRequest {
    cursor: Option<String>,
}
fn catalog_cursor(payload: &[u8]) -> Result<Option<String>, &'static str> {
    let request: CatalogRequest =
        serde_json::from_slice(payload).map_err(|_| "Invalid catalog request")?;
    if request.cursor.as_ref().is_some_and(|cursor| {
        cursor.is_empty() || cursor.len() > 4096
    }) {
        return Err("Invalid catalog cursor");
    }
    Ok(request.cursor)
}
fn json_error(status: u16, message: &str) -> TypedReply<ManagementResponse> {
    reply(
        status,
        serde_json::to_vec(&json!({"error": message})).expect("JSON error message"),
    )
}
// Only public catalog facts are returned. POST carries the opaque cursor as JSON.
async fn catalog_page(
    host: &HostClient,
    path: &str,
    cursor: Option<String>,
) -> Result<serde_json::Value, PluginFault> {
    if path == "api/keys" {
        let response = host
            .call(
                "host.keys.list",
                serde_json::to_value(KeyListRequest { cursor, limit: 200 })
                    .map_err(|_| fault("invalid key cursor"))?,
                vec![],
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        let page: KeyListResult = serde_json::from_value(response.result)
            .map_err(|_| fault("invalid key list"))?;
        Ok(json!({"items": page.keys, "next_cursor": page.next_cursor}))
    } else {
        let query = gateway_plugin_sdk::call::data::AccountFactsQuery {
            provider_id: Some("openai".into()),
            cursor,
            limit: 200,
        };
        let response = host
            .call(
                "host.data.accounts.list",
                json!({}),
                serde_json::to_vec(&query).map_err(|_| fault("invalid account cursor"))?,
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        let page: gateway_plugin_sdk::call::data::AccountFactsPage =
            serde_json::from_slice(&response.payload)
                .map_err(|_| fault("invalid account list"))?;
        Ok(json!({"items": page.accounts, "next_cursor": page.next_cursor}))
    }
}

pub fn registration(show_page: bool) -> ManagementRegistration {
    ManagementRegistration {
        routes: vec![
            ManagementRoute {
                method: "POST".into(),
                path: "api/keys".into(),
                request_content_types: vec!["application/json".into()],
                response_content_types: vec!["application/json".into()],
            },
            ManagementRoute {
                method: "POST".into(),
                path: "api/accounts".into(),
                request_content_types: vec!["application/json".into()],
                response_content_types: vec!["application/json".into()],
            },
            ManagementRoute {
                method: "GET".into(),
                path: "api/options".into(),
                request_content_types: vec![],
                response_content_types: vec!["application/json".into()],
            },
            ManagementRoute {
                method: "GET".into(),
                path: "api/plugin-info".into(),
                request_content_types: vec![],
                response_content_types: vec!["application/json".into()],
            },
            ManagementRoute {
                method: "GET".into(),
                path: "api/snapshot".into(),
                request_content_types: vec![],
                response_content_types: vec!["application/json".into()],
            },
            ManagementRoute {
                method: "POST".into(),
                path: "api/policy".into(),
                request_content_types: vec!["application/json".into()],
                response_content_types: vec!["application/json".into()],
            },
            ManagementRoute {
                method: "POST".into(),
                path: "api/observe".into(),
                request_content_types: vec!["application/json".into()],
                response_content_types: vec!["application/json".into()],
            },
        ],
        resources: ["web/index.html", "web/app.js", "web/app.css"]
            .into_iter()
            .map(|path| ManagementResource {
                path: path.into(),
                public: false,
            })
            .collect(),
        pages: if show_page {
            vec![ManagementPage {
                id: "excel-gateway".into(),
                title: "Excel 网关".into(),
                description: Some("请求监控与准入控制".into()),
                entry: "web/index.html".into(),
                icon: None,
            }]
        } else {
            vec![]
        },
        callbacks: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_cursor_is_opaque_and_input_is_bounded() {
        assert_eq!(catalog_cursor(br#"{"cursor":null}"#), Ok(None));
        assert_eq!(
            catalog_cursor(br#"{"cursor":"page-2_opaque"}"#),
            Ok(Some("page-2_opaque".into()))
        );
        assert!(catalog_cursor(br#"{"cursor":"","limit":9999}"#).is_err());
        let oversized = serde_json::to_vec(&json!({"cursor": "x".repeat(4097)})).unwrap();
        assert!(catalog_cursor(&oversized).is_err());
    }
    #[test]
    fn catalog_routes_are_read_only_and_keep_existing_routes() {
        let registration = registration(true);
        for path in ["api/keys", "api/accounts"] {
            assert!(registration.routes.iter().any(|route| {
                route.path == path && route.method == "POST"
            }));
        }
        assert!(registration.routes.iter().any(|route| {
            route.path == "api/options" && route.method == "GET"
        }));
        assert_eq!(registration.pages[0].id, "excel-gateway");
        assert!(super::registration(false).pages.is_empty());
    }
}
