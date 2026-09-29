use cpr_excel_companion::{admission::Policy, control::Control, stream::policy_server_error};
use serde_json::json;

#[test]
fn setting_is_default_off_and_accepts_older_saved_policy() {
    let default = Policy::default();
    assert!(!default.policy_errors_as_server_error);
    let mut old = serde_json::to_value(default).unwrap();
    old.as_object_mut().unwrap().remove("policy_errors_as_server_error");
    assert!(!serde_json::from_value::<Policy>(old).unwrap().policy_errors_as_server_error);
}

#[test]
fn mapping_only_recognizes_explicit_policy_classifications() {
    for code in ["cyber_policy", "bio_policy", "misalignment_policy_violation", "content_filter"] {
        for event in [
            json!({"error":{"code":code,"message":"private prompt Bearer secret"}}),
            json!({"type":"response.failed","response":{"error":{"code":code}}}),
            json!({"type":"error","code":code}),
            json!({"type":"error","error_code":code}),
            json!({"type":"error","error":{"code":"unknown_provider_code","type":code}}),
            json!({"type":"error","error_type":code}),
            json!({"type":"error","response":{"error_type":code}}),
        ] {
            let original = event.clone();
            let mapped = policy_server_error(&event).unwrap();
            assert_eq!(mapped["type"], "server_error");
            assert_eq!(mapped["code"], "server_error");
            assert!(!mapped.to_string().contains("private"));
            assert!(!mapped.to_string().contains(code));
            assert_eq!(event, original);
        }
    }
    for code in ["invalid_prompt", "context_length_exceeded", "rate_limit_exceeded", "invalid_api_key", "permission_denied", "unknown"] {
        assert!(policy_server_error(&json!({"error":{"code":code,"message":"cyber_policy"}})).is_none());
    }
    assert!(policy_server_error(&json!({"type":"response.incomplete","response":{"incomplete_details":{"reason":"content_filter"}}})).is_some());
    assert!(policy_server_error(&json!({"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}})).is_none());
}

#[test]
fn option_is_saved_and_reloaded_through_existing_control() {
    let path = std::env::temp_dir().join(format!("policy-mapping-{}.json", uuid::Uuid::new_v4()));
    let control = Control::load(path.clone()).unwrap();
    assert!(!control.policy_errors_as_server_error());
    let policy = Policy { policy_errors_as_server_error: true, ..Default::default() };
    control.save(policy, Some(0)).unwrap();
    assert!(control.policy_errors_as_server_error());
    assert!(Control::load(path.clone()).unwrap().policy_errors_as_server_error());
    std::fs::remove_file(path).unwrap();
}
