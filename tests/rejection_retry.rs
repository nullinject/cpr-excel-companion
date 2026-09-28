use cpr_excel_companion::{admission::Policy, stream::{RejectionRetry, RetryDecision}};
use serde_json::{Value, json};
fn created() -> Value { json!({"type":"response.created","response":{"id":"first"}}) }
fn blocked() -> Value { json!({"type":"error","error":{"code":"cyber_policy","type":"invalid_request_error","message":"private"}}) }
#[test]
fn legacy_policy_defaults_to_faithful_return() {
    let mut v=serde_json::to_value(Policy::default()).unwrap();
    v.as_object_mut().unwrap().remove("retry_policy_errors");
    let p:Policy=serde_json::from_value(v).unwrap();
    assert!(!p.retry_policy_errors);
    let mut window=RejectionRetry::new(p.retry_policy_errors);
    assert!(matches!(window.event(created()),RetryDecision::Forward(_)));
    assert!(matches!(window.event(blocked()),RetryDecision::Forward(_)));
}
#[test]
fn five_retries_discard_only_uncommitted_preludes() {
    let mut w=RejectionRetry::new(true);
    for attempt in 1..=5 {
        assert!(matches!(w.event(created()),RetryDecision::Hold));
        match w.event(blocked()) {
            RetryDecision::Retry(events) => { assert_eq!(events.len(),2);assert_eq!(events[1],blocked()); },
            _=>panic!("expected bounded retry"),
        }
        assert_eq!(w.retries(),attempt);
    }
    assert!(matches!(w.event(created()),RetryDecision::Forward(_)));
    assert!(matches!(w.event(blocked()),RetryDecision::Forward(_)));
    assert_eq!(w.retries(),5);
}
#[test]
fn output_tools_unknown_events_and_usage_prevent_replay() {
    for event in [
        json!({"type":"response.output_text.delta","delta":"text"}),
        json!({"type":"response.output_item.added","item":{"type":"reasoning"}}),
        json!({"type":"response.output_item.added","item":{"type":"function_call"}}),
        json!({"type":"response.function_call_arguments.delta","delta":"{"}),
        json!({"type":"provider.new_event"}),
    ] {
        let mut w=RejectionRetry::new(true);
        w.event(created());
        assert!(matches!(w.event(event),RetryDecision::Forward(_)));
        assert!(matches!(w.event(blocked()),RetryDecision::Forward(_)));
    }
    for field in ["usage","output"] {
        let mut w=RejectionRetry::new(true);w.event(created());
        let mut e=blocked();e["response"]=json!({});
        e["response"][field]=if field=="usage" {json!({"input_tokens":17})} else {json!([{"type":"function_call"}])};
        assert!(matches!(w.event(e),RetryDecision::Forward(_)));
    }
}
#[test]
fn only_explicit_policy_codes_are_eligible_and_buffer_is_bounded() {
    for code in ["invalid_prompt","server_overloaded","rate_limit_exceeded","excel_upstream_failed"] {
        let mut w=RejectionRetry::new(true);
        assert!(matches!(w.event(json!({"type":"error","error":{"code":code}})),RetryDecision::Forward(_)));
    }
    let mut w=RejectionRetry::new(true);
    let mut released=false;
    for _ in 0..100 {
        if matches!(w.event(created()),RetryDecision::Forward(_)) {released=true;break;}
    }
    assert!(released);
    assert!(matches!(w.event(blocked()),RetryDecision::Forward(_)));
}
