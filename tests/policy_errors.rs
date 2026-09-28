use cpr_excel_companion::{Tools, stream::{Translator, failure_details, failure_diagnostics}};
use serde_json::{Value, json};

const CODES: [&str; 7] = ["cyber_policy", "bio_policy", "misalignment_policy_violation", "invalid_prompt", "server_overloaded", "content_filter", "usage_not_included"];

#[test]
fn native_error_codes_survive_every_supported_error_shape() {
    for code in CODES {
        for event in [
            json!({"type":"error","error":{"code":code,"type":"invalid_request_error","message":"Bearer private-token"}}),
            json!({"type":"response.failed","response":{"error":{"code":code,"type":"server_error"}}}),
            json!({"type":"error","code":code}),
            json!({"type":"error","error_code":code}),
            json!({"type":"error","error":{"code":"unknown_provider_code","type":code}}),
            json!({"type":"error","error_type":code}),
            json!({"type":"error","response":{"error_type":code}}),
        ] {
            assert_eq!(failure_details(&event).0, code, "{event}");
            let mut translator = Translator::new(Tools::new());
            translator.event(json!({"type":"response.created","response":{"id":"resp_policy_test"}})).unwrap();
            let result = translator.event(event).unwrap();
            assert_eq!(result.len(), 1);
            assert_eq!(result[0]["type"], "response.failed");
            assert_eq!(result[0]["response"]["id"], "resp_policy_test");
            assert_eq!(result[0]["response"]["status"], "failed");
            assert_eq!(result[0]["response"]["error"]["code"], code);
            assert!(!result[0].to_string().contains("private-token"));
            assert!(translator.completed.is_none());
            assert!(translator.event(json!({"type":"response.completed"})).unwrap().is_empty());
        }
    }
}

#[test]
fn policy_failures_do_not_expose_partial_tools_or_private_messages() {
    let mut translator = Translator::new(Tools::new());
    let event = json!({"type":"response.failed","response":{
        "id":"resp_blocked","usage":{"input_tokens":19,"output_tokens":3},
        "error":{"code":"cyber_policy","message":"private prompt Bearer private-token"},
        "output":[{"type":"function_call","name":"run_officejs","arguments":"{unfinished"}]}});
    let diagnostic = failure_diagnostics(&event);
    assert_eq!(diagnostic["/response/error/code"]["recognized_code"], "cyber_policy");
    assert!(!diagnostic.to_string().contains("private"));
    let result = translator.event(event).unwrap();
    assert_eq!(result[0]["response"]["output"], json!([]));
    assert_eq!(result[0]["response"]["usage"]["input_tokens"], 19);
    assert_eq!(result[0]["response"]["error"]["type"], "invalid_request_error");
    assert!(translator.originals.is_empty());
    assert!(!result[0].to_string().contains("private"));
}

#[test]
fn unknown_errors_are_not_invented_as_policy_and_message_text_is_not_a_code() {
    for event in [
        json!({"type":"error","error":{"code":"unknown_private_code","message":"cyber_policy"}}),
        json!({"type":"error","error":{"code":null,"message":"private"}}),
        json!({"type":"error","error":{"code":"sk-private-token","type":"private_type"}}),
    ] {
        assert_eq!(failure_details(&event).0, "excel_upstream_failed");
        let result = Translator::new(Tools::new()).event(event).unwrap();
        assert!(!result[0].to_string().contains("private"));
    }
    let event: Value = json!({"type":"response.incomplete","response":{"incomplete_details":{"reason":"content_filter"}}});
    let result = Translator::new(Tools::new()).event(event).unwrap();
    assert_eq!(result[0]["type"], "response.incomplete");
    assert_eq!(result[0]["response"]["incomplete_details"]["reason"], "content_filter");
}
