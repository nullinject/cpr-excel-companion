//! Excel/Basispoints 的 Responses 协议转换；不读取本机登录、不执行工具。
pub mod admission;
pub mod attachments;
pub mod auth;
pub mod control;
pub mod history;
pub mod observe;
pub mod stream;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const ENDPOINT: &str = "https://bps.openai.com/basispoints/api/responses";
pub type Result<T> = std::result::Result<T, &'static str>;

fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
pub fn function_id(id: Option<&str>, call_id: &str) -> String {
    if let Some(id) = id.filter(|s| {
        s.starts_with("fc_")
            && s.len() <= 64
            && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    }) {
        return id.to_owned();
    }
    format!("fc_{}", &digest(call_id)[..61])
}

#[derive(Clone)]
pub struct Tool {
    pub name: String,
    pub namespace: Option<String>,
    pub custom: bool,
    pub spec: Value,
}
pub type Tools = BTreeMap<String, Tool>;

pub fn tool_catalog(source: &Value) -> Result<Tools> {
    match source.get("tool_choice") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) if s == "auto" => {}
        Some(Value::String(s)) if s == "none" => return Ok(Tools::new()),
        _ => return Err("only tool_choice auto and none are supported"),
    }
    fn add(items: &[Value], namespace: Option<&str>, out: &mut Tools) -> Result<()> {
        for item in items {
            let kind = item["type"].as_str().ok_or("tool type is required")?;
            let name = item["name"].as_str().ok_or("tool name is required")?;
            if name.is_empty() {
                return Err("empty tool name");
            }
            if kind == "namespace" {
                if namespace.is_some() {
                    return Err("nested namespaces are unsupported");
                }
                add(
                    item["tools"]
                        .as_array()
                        .ok_or("namespace tools are required")?,
                    Some(name),
                    out,
                )?;
                continue;
            }
            if !matches!(kind, "function" | "custom") {
                return Err(
                    "unsupported built-in tool; only function and custom tools are supported",
                );
            }
            let key = namespace.map_or_else(|| name.to_owned(), |ns| format!("{ns}.{name}"));
            if out
                .insert(
                    key,
                    Tool {
                        name: name.into(),
                        namespace: namespace.map(str::to_owned),
                        custom: kind == "custom",
                        spec: item.clone(),
                    },
                )
                .is_some()
            {
                return Err("duplicate tool name");
            }
        }
        Ok(())
    }
    let mut out = Tools::new();
    if let Some(items) = source.get("tools") {
        add(
            items.as_array().ok_or("tools must be an array")?,
            None,
            &mut out,
        )?;
    }
    Ok(out)
}

fn relay_call(item: &Value) -> Result<Value> {
    let call_id = item["call_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("tool call_id is required")?;
    let name = item["name"].as_str().ok_or("tool name is required")?;
    let key = item["namespace"]
        .as_str()
        .map_or_else(|| name.to_owned(), |ns| format!("{ns}.{name}"));
    let envelope = if item["type"] == "custom_tool_call" {
        json!({"name":key,"input":item["input"].as_str().ok_or("custom tool input must be text")?})
    } else {
        let args: Value = serde_json::from_str(
            item["arguments"]
                .as_str()
                .ok_or("function arguments must be JSON text")?,
        )
        .map_err(|_| "invalid function arguments")?;
        json!({"name":key,"arguments":args})
    };
    Ok(
        json!({"type":"function_call", "id":function_id(item["id"].as_str(), call_id), "call_id":call_id,
        "name":"run_officejs", "status":"completed", "arguments":json!({
            "summary":format!("Run client tool {key}"), "extended_summary":format!("Relay {key} to the external client"),
            "code":envelope.to_string(),"destructive":false,"references":[]
        }).to_string()}),
    )
}

/// original_calls 只能来自本账号此前的上游响应，不接受跨账号缓存。
pub fn prepare(source: &Value, original_calls: &BTreeMap<String, Value>) -> Result<Value> {
    if !source.is_object() {
        return Err("request must be an object");
    }
    for field in ["previous_response_id", "conversation"] {
        if source.get(field).is_some_and(|v| !v.is_null()) {
            return Err("native continuation is unsupported; send full input history");
        }
    }
    if source
        .pointer("/reasoning/mode")
        .and_then(Value::as_str)
        .is_some_and(|s| s != "standard")
    {
        return Err("unsupported reasoning mode");
    }
    if source
        .pointer("/text/format/type")
        .and_then(Value::as_str)
        .is_some_and(|s| s != "text")
    {
        return Err("structured output formats are not supported");
    }
    let tools = tool_catalog(source)?;
    let model = source["model"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("model is required")?;
    let model = model.strip_suffix("-excel").unwrap_or(model);
    let effort = source
        .pointer("/reasoning/effort")
        .and_then(Value::as_str)
        .or_else(|| source["reasoning_effort"].as_str())
        .unwrap_or("medium");
    if !["low", "medium", "high", "xhigh"].contains(&effort) {
        return Err("unsupported reasoning effort; no silent downgrade is performed");
    }
    let raw = match &source["input"] {
        Value::String(s) => {
            vec![json!({"type":"message","role":"user","content":[{"type":"input_text","text":s}]})]
        }
        Value::Array(a) => a.clone(),
        _ => return Err("input must be text or an array"),
    };
    let history_root = raw.first().map(Value::to_string).unwrap_or_default();
    let turn_end = raw
        .iter()
        .rposition(|item| item["role"] == "user")
        .map_or(raw.len(), |index| index + 1);
    let iteration = 1 + raw[turn_end..]
        .iter()
        .filter(|item| {
            item["type"]
                .as_str()
                .is_some_and(|kind| kind.ends_with("_call_output"))
        })
        .count();
    let turn_id = digest(&serde_json::to_string(&raw[..turn_end]).map_err(|_| "invalid history")?);
    let task_id = digest(source["prompt_cache_key"].as_str().unwrap_or(&history_root));
    let mut input = Vec::new();
    if let Some(instructions) = source["instructions"].as_str() {
        input.push(json!({"type":"message","role":"developer","content":[{"type":"input_text","text":instructions}]}));
    }
    let catalog: Vec<Value> = tools
        .iter()
        .map(|(name, t)| {
            let mut spec = t.spec.clone();
            spec["name"] = json!(name);
            spec
        })
        .collect();
    let guidance = if tools.is_empty() {
        "This is an external Responses client, not a live Excel workbook. Answer in text; do not invoke workbook tools.".to_owned()
    } else {
        format!(
            "This is an external Responses client. Native run_officejs is an intercepted transport, not an OfficeJS executor. For each client tool, call run_officejs with summary, extended_summary, destructive=false, references=[], and code containing one JSON string: {{\"name\":\"CATALOG_NAME\",\"arguments\":{{}}}} for function tools, or {{\"name\":\"CATALOG_NAME\",\"input\":\"RAW_TEXT\"}} for custom tools. Do not execute OfficeJS or invoke other native tools. Preserve names and schemas exactly. {} Catalog: {}",
            if source["parallel_tool_calls"] == false {
                "Make only one tool call per response."
            } else {
                "Independent tools may be separate run_officejs calls in one response."
            },
            json!(catalog)
        )
    };
    input.push(json!({"type":"message","role":"developer","content":[{"type":"input_text","text":guidance}]}));
    for mut item in raw {
        let obj = item.as_object_mut().ok_or("input items must be objects")?;
        obj.remove("internal_chat_message_metadata_passthrough");
        match item["type"].as_str().unwrap_or("message") {
            "function_call" | "custom_tool_call" => {
                let id = item["call_id"].as_str().ok_or("tool call_id is required")?;
                input.push(match original_calls.get(id) {
                    Some(original) => original.clone(),
                    None => relay_call(&item)?,
                });
            }
            "function_call_output" | "custom_tool_call_output" => {
                let call_id = item["call_id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or("tool result call_id is required")?;
                let id = function_id(item["id"].as_str(), call_id);
                item["type"] = json!("function_call_output");
                item["id"] = json!(id);
                input.push(item);
            }
            "reasoning" => {
                if let Some(encrypted) =
                    item["encrypted_content"].as_str().filter(|s| !s.is_empty())
                {
                    input.push(
                        json!({"type":"reasoning","summary":[],"encrypted_content":encrypted}),
                    );
                }
            }
            "item_reference" => {
                return Err("item_reference requires unavailable upstream stored state");
            }
            _ => {
                input.push(item);
            }
        }
    }
    let mut result = json!({"model":model,"model_selection":"explicit","store":false,"stream":true,"input":input,"reasoning_effort":effort});
    result["context_management"] = json!([{"type":"compaction","compact_threshold":200000}]);
    result["metadata"] =
        json!({"task_id":task_id,"turn_id":turn_id,"agent_iteration":iteration.to_string()});
    for field in ["prompt_cache_key", "context_management"] {
        if let Some(value) = source.get(field) {
            result[field] = value.clone();
        }
    }
    Ok(result)
}

pub fn restore_call(native: &Value, tools: &Tools) -> Result<Value> {
    if !matches!(
        native["name"].as_str(),
        Some("run_officejs" | "functions.run_officejs")
    ) {
        return Err("unexpected native tool; no tool is executed by this plugin");
    }
    let args: Value = serde_json::from_str(
        native["arguments"]
            .as_str()
            .ok_or("missing native arguments")?,
    )
    .map_err(|_| "invalid native arguments")?;
    let envelope: Value =
        serde_json::from_str(args["code"].as_str().ok_or("missing tool transport code")?)
            .map_err(|_| "invalid tool transport envelope")?;
    let tool = tools
        .get(
            envelope["name"]
                .as_str()
                .ok_or("missing client tool name")?,
        )
        .ok_or("upstream requested an undeclared tool")?;
    let call_id = native["call_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("missing native call_id")?;
    let mut output = json!({"type":if tool.custom {"custom_tool_call"} else {"function_call"},"name":tool.name,"call_id":call_id,"status":"completed","id":function_id(native["id"].as_str(),call_id)});
    if let Some(ns) = &tool.namespace {
        output["namespace"] = json!(ns);
    }
    if tool.custom {
        output["id"] = json!(format!("ctc_{}", &digest(call_id)[..40]));
        output["input"] = json!(
            envelope["input"]
                .as_str()
                .ok_or("custom input must be text")?
        );
    } else {
        let args = match &envelope["arguments"] {
            Value::String(s) => serde_json::from_str(s).map_err(|_| "invalid inner arguments")?,
            v => v.clone(),
        };
        if !args.is_object() {
            return Err("function arguments must be an object");
        }
        output["arguments"] = json!(args.to_string());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source() -> Value {
        json!({"model":"gpt-5.6-sol-excel","input":"hello","tools":[{"type":"function","name":"shell","parameters":{"type":"object"}}]})
    }
    #[test]
    fn deterministic_request_and_no_silent_effort_downgrade() {
        let s = source();
        let a = prepare(&s, &BTreeMap::new()).unwrap();
        assert_eq!(a, prepare(&s, &BTreeMap::new()).unwrap());
        assert_eq!(a["model"], "gpt-5.6-sol");
        assert!(a.get("tools").is_none());
        let mut s = s;
        s["reasoning"] = json!({"effort":"max"});
        assert!(prepare(&s, &BTreeMap::new()).is_err());
    }
    #[test]
    fn tool_result_ids_preserve_valid_and_fix_invalid() {
        assert_eq!(function_id(Some("fc_valid"), "call_1"), "fc_valid");
        let a = function_id(Some("ctc_bad"), "call_1");
        assert!(a.starts_with("fc_"));
        assert_eq!(a, function_id(None, "call_1"));
        assert!(a.len() <= 64);
        let mut s = source();
        s["input"] = json!([{"type":"custom_tool_call_output","call_id":"call_1","id":"ctc_bad","output":""}]);
        let r = prepare(&s, &BTreeMap::new()).unwrap();
        let item = r["input"].as_array().unwrap().last().unwrap();
        assert_eq!(item["output"], "");
        assert_eq!(item["type"], "function_call_output");
        assert_eq!(item["call_id"], "call_1");
    }
    #[test]
    fn tool_round_trip_preserves_call_and_payload() {
        let item = json!({"type":"function_call","id":"fc_original","call_id":"call_1","name":"shell","arguments":"{\"cmd\":\"echo 你好\"}"});
        let native = relay_call(&item).unwrap();
        let restored = restore_call(&native, &tool_catalog(&source()).unwrap()).unwrap();
        assert_eq!(restored["arguments"], item["arguments"]);
        assert_eq!(restored["id"], item["id"]);
        assert_eq!(restored["call_id"], item["call_id"]);
    }
    #[test]
    fn undeclared_tools_and_native_continuation_are_rejected() {
        let mut s = source();
        s["previous_response_id"] = json!("resp_x");
        assert!(prepare(&s, &BTreeMap::new()).is_err());
        let n = json!({"name":"run_officejs","call_id":"a","arguments":json!({"code":"{\"name\":\"unlisted\",\"arguments\":{}}"}).to_string()});
        assert!(restore_call(&n, &tool_catalog(&source()).unwrap()).is_err());
    }
}

#[cfg(test)]
mod request_regression {
    use super::*;
    #[test]
    fn none_disables_tools_and_forced_choices_are_rejected() {
        let mut source = json!({"model":"gpt-5.6-sol-excel","input":"hello","tools":[{"type":"function","name":"shell"}],"tool_choice":"none"});
        assert!(tool_catalog(&source).unwrap().is_empty());
        source["tool_choice"] = json!("required");
        assert!(prepare(&source, &BTreeMap::new()).is_err());
    }
    #[test]
    fn structured_format_is_not_silently_ignored() {
        let source = json!({"model":"gpt-5.6-sol-excel","input":"hello","text":{"format":{"type":"json_schema"}}});
        assert!(prepare(&source, &BTreeMap::new()).is_err());
    }
}

type NativeCache = std::sync::Mutex<BTreeMap<(String, String), (std::time::Instant, Value)>>;
fn native_cache() -> &'static NativeCache {
    static CACHE: std::sync::OnceLock<NativeCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(NativeCache::default)
}
pub fn cache_load(scope: &str) -> BTreeMap<String, Value> {
    let mut cache = native_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.retain(|_, (created, _)| created.elapsed() < std::time::Duration::from_secs(1800));
    cache
        .iter()
        .filter(|((owner, _), _)| owner == scope)
        .map(|((_, id), (_, value))| (id.clone(), value.clone()))
        .collect()
}
pub fn cache_save(scope: &str, items: &BTreeMap<String, Value>) {
    let mut cache = native_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (id, item) in items {
        if serde_json::to_vec(item).is_ok_and(|bytes| bytes.len() <= 65536) {
            cache.insert(
                (scope.to_owned(), id.clone()),
                (std::time::Instant::now(), item.clone()),
            );
        }
    }
    while cache.len() > 1024 {
        let oldest = cache
            .iter()
            .min_by_key(|(_, (created, _))| created)
            .map(|(key, _)| key.clone());
        if let Some(key) = oldest {
            cache.remove(&key);
        } else {
            break;
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("{0}")]
    ExcelRequest(&'static str),
    #[error("upstream transport failed")]
    Http(#[from] reqwest::Error),
    #[error("invalid upstream header")]
    Header(#[from] reqwest::header::InvalidHeaderValue),
}
pub type BridgeResult<T> = std::result::Result<T, BridgeError>;
