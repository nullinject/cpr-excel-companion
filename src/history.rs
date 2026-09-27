//! 有界的进程内续接重放；按宿主认证的账户和 Client Key 隔离。
use super::{Result, Value, json};
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

type Cache = BTreeMap<(String, String), (Instant, Vec<Value>)>;
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
    let (_, prior) = cache.get(&(scope.to_owned(), previous.to_owned())).ok_or(
        "Excel continuation expired or belongs to another account or key; send full history",
    )?;
    let mut history = prior.clone();
    history.extend(input(source)?);
    if serde_json::to_vec(&history).map_or(true, |bytes| bytes.len() > 2 * 1024 * 1024) {
        return Err("Excel continuation history exceeds 2 MiB; start a new conversation");
    }
    source["input"] = Value::Array(history);
    source
        .as_object_mut()
        .ok_or("invalid request")?
        .remove("previous_response_id");
    Ok(())
}

pub fn save(scope: &str, input: &[Value], response: &Value) {
    if response["status"] != "completed" {
        return;
    }
    let (Some(id), Some(output)) = (response["id"].as_str(), response["output"].as_array()) else {
        return;
    };
    let mut history = input.to_vec();
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
    fn prewarm_never_becomes_a_billable_generation() {
        assert!(restore("account/key", &mut json!({"generate":false,"input":"hi"})).is_err());
    }
}
