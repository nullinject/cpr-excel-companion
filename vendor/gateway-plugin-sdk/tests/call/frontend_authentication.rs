//! 验证插件认证合同限制身份选择并脱敏凭据和主体

use gateway_plugin_sdk::call::frontend_authentication::{
    FrontendAuthenticationRequest, FrontendAuthenticationResult,
};
use serde_json::json;

#[test]
fn authentication_result_only_consumes_the_principal_and_ignores_extra_fields() {
    let result = serde_json::from_value::<FrontendAuthenticationResult>(json!({
        "outcome":"authenticated",
        "principal":"controlled-principal",
        "client_key_id":"attacker-selected-key"
    }));
    assert_eq!(
        result.unwrap(),
        FrontendAuthenticationResult::Authenticated {
            principal: "controlled-principal".into(),
        }
    );
}

#[test]
fn authentication_debug_output_redacts_credentials_and_principals() {
    let request = FrontendAuthenticationRequest {
        authorization: "Bearer controlled-secret".into(),
    };
    let result = FrontendAuthenticationResult::Authenticated {
        principal: "private-principal".into(),
    };
    let request_debug = format!("{request:?}");
    let result_debug = format!("{result:?}");
    assert!(!request_debug.contains("controlled-secret"));
    assert!(!result_debug.contains("private-principal"));
}
