use gateway_plugin_sdk::{Frame, Message};

#[test]
fn diagnostics_do_not_render_peer_payloads_or_error_details() {
    let secret = "sensitive-fixture-value";
    let fault = gateway_plugin_sdk::PluginFault::new(gateway_plugin_sdk::ErrorCode::Fault, secret);
    let frame = Frame {
        message: Message::Result {
            id: 1,
            result: serde_json::json!({"credential":secret}),
        },
        payload: secret.as_bytes().to_vec(),
    };
    assert!(!format!("{frame:?} {fault:?}").contains(secret));
}
