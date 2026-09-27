use gateway_plugin_sdk::call::observation::ObserveWebSocketResponse;
use serde_json::json;

#[test]
fn websocket_response_observation_keeps_payload_out_of_json_and_rejects_unknown_fields() {
    let event = ObserveWebSocketResponse {
        event_id: "req_observed:websocket:2:7".into(),
        request_id: "req_observed".into(),
        config_revision: 9,
        operation: "generate".into(),
        protocol: "openai".into(),
        provider: "openai".into(),
        attempt_index: 2,
        sequence: 7,
        payload_included: false,
        requested_model: Some("public-model".into()),
        account_id: None,
        event_type: None,
    };
    let encoded = serde_json::to_value(&event).unwrap();
    assert_eq!(encoded["payload_included"], false);
    assert!(encoded.get("account_id").is_none());
    assert!(encoded.get("event_type").is_none());
    assert!(encoded.get("headers").is_none());
    assert!(encoded.get("body").is_none());
    assert_eq!(
        serde_json::from_value::<ObserveWebSocketResponse>(encoded).unwrap(),
        event
    );

    assert!(
        serde_json::from_value::<ObserveWebSocketResponse>(json!({
            "event_id":"req_observed:websocket:2:7",
            "request_id":"req_observed",
            "config_revision":9,
            "operation":"generate",
            "protocol":"openai",
            "provider":"openai",
            "attempt_index":2,
            "sequence":7,
            "payload_included":false,
            "content_hint":"must not be accepted"
        }))
        .is_err()
    );
}
