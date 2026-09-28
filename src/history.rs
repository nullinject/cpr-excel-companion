//! 有界的进程内续接重放；按宿主认证的账户和 Client Key 隔离。
use super::{Result, Value, json};
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

type Cache = BTreeMap<(String, String), (Instant, Vec<Value>)>;
pub const MISSING: &str = "Excel continuation expired or belongs to another account or key; send full history";
pub const CACHE_LIMIT: &str = "Excel continuation exceeds the 2 MiB cache budget; resend full input history without previous_response_id";
fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

pub fn input(source: &Value) -> Result<Vec<Value>> {
    match &source["input"] {
        Value::String(text) => Ok(vec![
            json!({"role":"user","content":[{"type":"input_text","text":text}]}),
        ]),
        Value::Array(items) => Ok(items.clone()),
        _ => Err("Excel input must be text or an array"),
    }
}

pub fn restore(scope: &str, source: &mut Value) -> Result<()> {
    if source.get("generate") == Some(&Value::Bool(false)) {
        return Err("Excel does not support generate=false prewarming");
    }
    let Some(previous) = source.get("previous_response_id").filter(|v| !v.is_null()) else {
        return Ok(());
    };
    let previous = previous.as_str().ok_or("invalid previous_response_id")?;
    let mut cache = cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.retain(|_, (created, _)| created.elapsed() < Duration::from_secs(1800));
    let (_, prior) = cache.get(&(scope.to_owned(), previous.to_owned())).ok_or(MISSING)?;
    let mut history = prior.clone();
    history.extend(input(source)?);
    if serde_json::to_vec(&history).map_or(true, |bytes| bytes.len() > 2 * 1024 * 1024) {
        return Err(CACHE_LIMIT);
    }
    source["input"] = Value::Array(history);
    source
        .as_object_mut()
        .ok_or("invalid request")?
        .remove("previous_response_id");
    Ok(())
}

/// A Codex connection prewarm reserves replay history without contacting Excel.
pub fn prewarm(scope: &str, source: &Value) -> Result<Vec<Value>> {
    if source.get("generate") != Some(&Value::Bool(false)) {
        return Err("prewarm requires generate=false");
    }
    let mut source = source.clone();
    source
        .as_object_mut()
        .ok_or("invalid request")?
        .remove("generate");
    restore(scope, &mut source)?;
    let input = input(&source)?;
    // The request decoder still enforces 32 MiB. The 2 MiB limit belongs
    // to the replay cache, not the conversation: save() skips large entries,
    // and a later delta gets the normal full-history replay signal.
    let model = source["model"].as_str().ok_or("model is required")?;
    let response = json!({
        "id": format!("resp_{}", uuid::Uuid::new_v4().simple()),
        "object": "response",
        "created_at": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
        "status": "completed", "model": model.strip_suffix("-excel").unwrap_or(model),
        "output": [], "error": null, "incomplete_details": null,
        "usage": {"input_tokens":0,"output_tokens":0,"total_tokens":0,
            "input_tokens_details":{"cached_tokens":0},"output_tokens_details":{"reasoning_tokens":0}}
    });
    save(scope, &input, &response);
    let mut started = response.clone();
    started["status"] = json!("in_progress");
    started["usage"] = Value::Null;
    Ok(vec![
        json!({"type":"response.created","sequence_number":0,"response":started}),
        json!({"type":"response.in_progress","sequence_number":1,"response":started}),
        json!({"type":"response.completed","sequence_number":2,"response":response}),
    ])
}

pub fn save(scope: &str, input: &[Value], response: &Value) {
    if response["status"] != "completed" {
        return;
    }
    let (Some(id), Some(output)) = (response["id"].as_str(), response["output"].as_array()) else {
        return;
    };
    // One-shot request controls are not conversation history. Keep actual
    // compaction output (including its encrypted content) for continuation.
    let mut history: Vec<Value> = input
        .iter()
        .filter(|item| item["type"] != "compaction_trigger")
        .cloned()
        .collect();
    history.extend(output.iter().cloned());
    if serde_json::to_vec(&history).map_or(true, |bytes| bytes.len() > 2 * 1024 * 1024) {
        return;
    }
    let mut cache = cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.retain(|_, (created, _)| created.elapsed() < Duration::from_secs(1800));
    if cache.len() >= 32 {
        let oldest = cache
            .iter()
            .min_by_key(|(_, (created, _))| *created)
            .map(|(key, _)| key.clone());
        if let Some(oldest) = oldest {
            cache.remove(&oldest);
        }
    }
    cache.insert((scope.to_owned(), id.to_owned()), (Instant::now(), history));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_prewarm_is_stateless_and_does_not_block_full_replay() {
        let source = json!({"model":"gpt-6-sol","generate":false,"input":"x".repeat(2 * 1024 * 1024)});
        let events = prewarm("oversized-prewarm", &source).unwrap();
        let response = &events[2]["response"];
        assert_eq!(response["usage"]["total_tokens"], 0);
        let mut delta = json!({"previous_response_id":response["id"],"input":"next"});
        assert!(restore("oversized-prewarm", &mut delta).is_err());
        let mut full = source.clone();
        full.as_object_mut().unwrap().remove("generate");
        restore("oversized-prewarm", &mut full).unwrap();
        assert_eq!(full["input"], source["input"]);
    }
    #[test]
    fn replay_keeps_history_and_isolates_keys() {
        save(
            "account/key-a",
            &[json!({"role":"user","content":"remember 123"})],
            &json!({"id":"resp_history_test","status":"completed","output":[{"role":"assistant","content":[{"type":"output_text","text":"123"}]}]}),
        );
        let mut next = json!({"previous_response_id":"resp_history_test","input":"repeat"});
        assert!(restore("account/key-b", &mut next).is_err());
        restore("account/key-a", &mut next).unwrap();
        assert_eq!(next["input"].as_array().unwrap().len(), 3);
        assert!(next.get("previous_response_id").is_none());
    }
    #[test]
    fn prewarm_has_zero_usage_and_replayable_input() {
        let source = json!({"model":"gpt-6-sol","generate":false,"input":"remember marker"});
        let events = prewarm("prewarm-test/account-key", &source).unwrap();
        assert_eq!(events.len(), 3);
        let response = &events[2]["response"];
        assert_eq!(response["usage"]["total_tokens"], 0);
        assert_eq!(response["output"], json!([]));
        let mut next = json!({"previous_response_id":response["id"],"input":"repeat marker"});
        assert!(restore("prewarm-test/other-key", &mut next).is_err());
        restore("prewarm-test/account-key", &mut next).unwrap();
        assert_eq!(next["input"].as_array().unwrap().len(), 2);
        assert_eq!(source["generate"], false);
        assert!(prewarm("a", &json!({"generate":true,"input":[]})).is_err());
    }
    #[test]
    fn prewarm_never_becomes_a_billable_generation() {
        assert!(restore("account/key", &mut json!({"generate":false,"input":"hi"})).is_err());
    }
}
