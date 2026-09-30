use cpr_excel_companion::{prepare, restore_call, tool_catalog};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn prepare_call(call: Value, cache: &BTreeMap<String, Value>) -> Value {
    let body = prepare(&json!({"model":"gpt-5.6-luna-excel","input":[call]}), cache).unwrap();
    body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call")
        .unwrap()
        .clone()
}

#[test]
fn native_transport_history_is_not_wrapped_again_after_cache_miss() {
    let arguments = r#"{ "references":["echo"], "code":"{}", "destructive":false }"#;
    for (name, namespace) in [
        ("run_officejs", None),
        ("functions.run_officejs", None),
        ("run_officejs", Some("functions")),
    ] {
        let mut call = json!({"type":"function_call","id":"fc_existing","call_id":"call_existing","name":name,"arguments":arguments});
        if let Some(namespace) = namespace {
            call["namespace"] = json!(namespace);
        }
        let output = prepare_call(call, &BTreeMap::new());
        assert_eq!(output["name"], "run_officejs");
        assert_eq!(output["id"], "fc_existing");
        assert_eq!(output["call_id"], "call_existing");
        assert_eq!(output["arguments"], arguments);
        assert!(output.get("namespace").is_none());
    }
}

#[test]
fn restored_function_and_custom_calls_are_marked_plaintext() {
    for custom in [false, true] {
        let spec = if custom {
            json!({"type":"custom","name":"echo","format":{"type":"text"}})
        } else {
            json!({"type":"function","name":"echo","parameters":{"type":"object"}})
        };
        let tools = tool_catalog(&json!({"tools":[spec]}));
        let native = json!({"type":"function_call","name":"run_officejs","call_id":"call_plain","arguments":json!({"references":["echo"],"code":if custom {"hello"} else {"{}"}}).to_string()});
        let output = restore_call(&native, &tools).unwrap();
        assert_eq!(output["encrypted_function_args"], json!([]));
        let replay = prepare_call(output, &BTreeMap::new());
        assert!(replay.get("encrypted_function_args").is_none());
    }
}

#[test]
fn native_plaintext_marker_is_stripped_but_cached_ciphertext_is_not() {
    let call = json!({"type":"function_call","call_id":"call_plain","name":"run_officejs","arguments":"{}","encrypted_function_args":[]});
    assert!(
        prepare_call(call, &BTreeMap::new())
            .get("encrypted_function_args")
            .is_none()
    );
    let native = json!({"type":"function_call","id":"fc_original","call_id":"call_cached","name":"run_officejs","arguments":"opaque ciphertext","encrypted_function_args":["opaque"]});
    let cache = BTreeMap::from([("call_cached".into(), native.clone())]);
    let client = json!({"type":"function_call","call_id":"call_cached","name":"echo","arguments":"{}","encrypted_function_args":[]});
    assert_eq!(prepare_call(client, &cache), native);
}

#[test]
fn native_transport_does_not_skip_argument_or_call_id_validation() {
    for arguments in ["[1]", "null", "not JSON"] {
        let source = json!({"model":"gpt-5.6-luna","input":[{"type":"function_call","call_id":"call_bad","name":"run_officejs","arguments":arguments}]});
        assert!(prepare(&source, &BTreeMap::new()).is_err());
    }
    let source = json!({"model":"gpt-5.6-luna","input":[{"type":"function_call","call_id":"","name":"run_officejs","arguments":"{}"}]});
    assert!(prepare(&source, &BTreeMap::new()).is_err());
}

#[test]
fn regular_client_tools_still_use_one_transport_envelope() {
    let call = json!({"type":"function_call","call_id":"call_client","name":"echo","namespace":"functions","arguments":"{\"text\":\"OK\"}"});
    let output = prepare_call(call, &BTreeMap::new());
    let arguments: Value = serde_json::from_str(output["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(output["name"], "run_officejs");
    assert_eq!(arguments["references"], json!(["functions.echo"]));
    assert_eq!(arguments["code"], "{\"text\":\"OK\"}");
}
