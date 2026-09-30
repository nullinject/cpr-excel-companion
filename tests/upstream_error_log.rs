use cpr_excel_companion::error_log::{ErrorLog, sanitize};
use serde_json::json;

#[test]
fn preserves_unknown_error_and_complete_multiline_message() {
    let message = format!("错误细节\n{}\n末尾", "detail ".repeat(4096));
    let event = json!({"type":"error","error":{"code":"new_provider_code_2026","type":"invalid_request_error","message":message,"param":"input[7].content","details":{"expected":"tool_output","extra":[1,2,3]}}});
    assert_eq!(sanitize(&event, &[]), event);
}

#[test]
fn redacts_credentials_without_dropping_error_fields() {
    let event = json!({"error":{"code":"invalid_argument","message":"Request rejected; Authorization: Bearer fixture_bearer_value","param":"messages","api_key":"fixture_api_key","nested":{"password":"fixture_password","salt":"fixture_salt","detail":"known credential fixture_known_value"}}});
    let actual = sanitize(&event, &["fixture_known_value"]);
    let text = actual.to_string();
    for secret in [
        "fixture_bearer_value",
        "fixture_api_key",
        "fixture_password",
        "fixture_salt",
        "fixture_known_value",
    ] {
        assert!(!text.contains(secret), "credential leaked");
    }
    assert_eq!(actual["error"]["code"], "invalid_argument");
    assert_eq!(actual["error"]["param"], "messages");
}

#[test]
fn records_event_and_http_failure_in_private_json_lines() {
    let dir = std::env::temp_dir().join(format!("cpr-error-log-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("errors.jsonl");
    let _ = std::fs::remove_file(&path);
    let log = ErrorLog::new(path.clone());
    let body = json!({"type":"error","error":{"code":"new_provider_code","message":"full detail","param":"input"}});
    log.record_event("req_fixture", 200, &body, &[]).unwrap();
    log.record_http(
        "req_fixture_http",
        400,
        "{\"error\":{\"code\":\"new_http_code\",\"message\":\"full HTTP detail\"}}",
        &[],
    )
    .unwrap();
    let data = std::fs::read_to_string(&path).unwrap();
    let records: Vec<serde_json::Value> = data
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["request_id"], "req_fixture");
    assert_eq!(records[0]["upstream_event"], body);
    assert_eq!(records[1]["http_status"], 400);
    assert_eq!(
        records[1]["upstream_body"]["error"]["code"],
        "new_http_code"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn redacts_structured_secrets_in_http_body_and_text() {
    let dir = std::env::temp_dir().join(format!("http-secrets-{}", uuid::Uuid::new_v4()));
    let path = dir.join("errors.jsonl");
    let log = ErrorLog::new(path.clone());
    let body=json!({"error":{"code":"bad_parameter","message":"ordinary error","details":{"refresh_token":"fixture_refresh","client_secret":"fixture_secret","Cookie":"fixture_cookie"}}}).to_string();
    log.record_http("req_http_secret", 400, &body, &[]).unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    for secret in ["fixture_refresh", "fixture_secret", "fixture_cookie"] {
        assert!(!text.contains(secret));
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn excludes_non_error_output_without_losing_nested_error_details() {
    let dir = std::env::temp_dir().join(format!("failed-response-{}", uuid::Uuid::new_v4()));
    let path = dir.join("errors.jsonl");
    let log = ErrorLog::new(path.clone());
    let event = json!({"type":"response.failed","response":{"id":"resp_fixture","status":"failed","output":[{"text":"unrelated full output"}],"error":{"code":"provider_unknown","message":"complete reason","details":{"field":"tools[0]"}}}});
    log.record_event("req_failed", 200, &event, &[]).unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    let record: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
    assert!(!text.contains("unrelated full output"));
    assert_eq!(
        record["upstream_event"]["response"]["error"],
        event["response"]["error"]
    );
    assert_eq!(
        record["omitted_non_error_fields"],
        json!(["/response/output"])
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn redacts_credential_assignments_embedded_in_messages() {
    let event = json!({"error":{"message":"details: {\"token\":\"fixture_nested_token\",\"api_key\":\"fixture_nested_key\"}; password='fixture_quoted_password'; URL https://fixture_user:fixture_url_password@localhost/path","authToken":"fixture_auth_token"}});
    let text = sanitize(&event, &[]).to_string();
    for secret in [
        "fixture_nested_token",
        "fixture_nested_key",
        "fixture_quoted_password",
        "fixture_url_password",
        "fixture_auth_token",
    ] {
        assert!(!text.contains(secret), "embedded credential leaked");
    }
}
