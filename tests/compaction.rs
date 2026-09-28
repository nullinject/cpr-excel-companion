use cpr_excel_companion::{history, prepare};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn request(input: Value) -> Value {
    json!({"model":"gpt-5.6-sol-excel","input":input,"tools":[{
        "type":"custom","name":"functions.exec","description":"Synthetic regression tool","format":{"type":"text"}
    }]})
}

#[test]
fn compaction_trigger_remains_final_after_tool_guidance() {
    let trigger=json!({"type":"compaction_trigger"});
    let source=request(json!([{"role":"user","content":"remember compaction marker"},trigger]));
    let before=source.clone();
    let prepared=prepare(&source,&BTreeMap::new()).unwrap();
    let input=prepared["input"].as_array().unwrap();
    assert_eq!(input.last(),Some(&trigger),"compaction_trigger must remain the final upstream input item");
    assert!(input[..input.len()-1].iter().any(|item| item.to_string().contains("Tool routing reminder")));
    assert_eq!(source,before,"source must not be mutated");
}

#[test]
fn compaction_trigger_is_not_replayed_as_conversation_history() {
    let scope="compaction-trigger-replay-regression";
    let first=json!([{"role":"user","content":"remember marker"},{"type":"compaction_trigger"}]);
    let compacted=json!({"type":"compaction","id":"cmp_regression","encrypted_content":"synthetic-encrypted-compaction"});
    history::save(scope,first.as_array().unwrap(),&json!({"id":"resp_compaction_regression","status":"completed","output":[compacted]}));
    let mut next=request(json!([{"role":"user","content":"continue"}]));
    next["previous_response_id"]=json!("resp_compaction_regression");
    history::restore(scope,&mut next).unwrap();
    let prepared=prepare(&next,&BTreeMap::new()).unwrap();
    let input=prepared["input"].as_array().unwrap();
    assert!(!input.iter().any(|item| item["type"]=="compaction_trigger"),"one-shot request trigger must not leak into replay history");
    assert!(input.contains(&compacted),"actual encrypted compaction result must be preserved");
}

#[test]
fn request_without_trigger_keeps_existing_tail_guidance() {
    let source=request(json!([{"role":"user","content":"ordinary request"}]));
    let prepared=prepare(&source,&BTreeMap::new()).unwrap();
    assert!(prepared["input"].as_array().unwrap().last().unwrap().to_string().contains("Tool routing reminder"));
}

#[test]
fn trigger_without_exec_tool_is_preserved() {
    let source=json!({"model":"gpt-5.6-sol-excel","input":[{"role":"user","content":"compact"},{"type":"compaction_trigger"}]});
    let prepared=prepare(&source,&BTreeMap::new()).unwrap();
    assert_eq!(prepared["input"].as_array().unwrap().last().unwrap()["type"],"compaction_trigger");
}
