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

pub fn tool_catalog(source: &Value) -> Tools {
    // 与原版/CPA 插件对齐：只有 tool_choice=none 清空目录；其余值按 auto 处理。
    // 只收割 function/custom（含 namespace 递归与输入历史 additional_tools），
    // 其他类型（web_search/local_shell 等）上游不可执行，静默跳过。
    let mut out = Tools::new();
    let none = source
        .get("tool_choice")
        .and_then(Value::as_str)
        .is_some_and(|s| s.eq_ignore_ascii_case("none"));
    if !none {
        fn add(items: &Value, namespace: Option<&str>, out: &mut Tools) {
            let Some(list) = items.as_array() else { return };
            for item in list {
                let kind = item["type"].as_str().unwrap_or("").trim().to_lowercase();
                let name = item["name"].as_str().unwrap_or("").trim();
                if (kind == "function" || kind == "custom") && !name.is_empty() {
                    let key =
                        namespace.map_or_else(|| name.to_owned(), |ns| format!("{ns}.{name}"));
                    out.insert(
                        key,
                        Tool {
                            name: name.to_owned(),
                            namespace: namespace.map(str::to_owned),
                            custom: kind == "custom",
                            spec: item.clone(),
                        },
                    );
                } else if kind == "namespace" && !name.is_empty() {
                    add(&item["tools"], Some(name), out);
                }
            }
        }
        add(&source["tools"], None, &mut out);
        // Codex 0.158+ 把动态工具目录放进输入历史的 additional_tools 项；
        // 后续声明覆盖同名工具。
        if let Some(items) = source["input"].as_array() {
            for item in items {
                if item["type"].as_str().is_some_and(|k| k.eq_ignore_ascii_case("additional_tools")) {
                    add(&item["tools"], None, &mut out);
                }
            }
        }
    }
    out
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
    // CPA/Basis Points selects the client tool through `references` and puts
    // only its payload in `code`. The previous Rust port emitted an empty
    // references list and nested {name, arguments/input} in code, which CPA
    // rejects as `invalid_tool_call`.
    let payload = if item["type"] == "custom_tool_call" {
        item["input"]
            .as_str()
            .ok_or("custom tool input must be text")?
            .to_owned()
    } else {
        let args: Value = serde_json::from_str(
            item["arguments"]
                .as_str()
                .ok_or("function arguments must be JSON text")?,
        )
        .map_err(|_| "invalid function arguments")?;
        if !args.is_object() {
            return Err("function arguments must be an object");
        }
        args.to_string()
    };
    Ok(
        json!({"type":"function_call", "id":function_id(item["id"].as_str(), call_id), "call_id":call_id,
        "name":"run_officejs", "status":"completed", "arguments":json!({
            "summary":format!("Run client tool {key}"), "extended_summary":format!("Relay {key} to the external client"),
            "code":payload,"destructive":false,"references":[key]
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
    let tools = tool_catalog(source);
    let model = source["model"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("model is required")?;
    let model = model.strip_suffix("-excel").unwrap_or(model);
    let effort = source
        .pointer("/reasoning/effort")
        .and_then(Value::as_str)
        .or_else(|| source["reasoning_effort"].as_str())
        .map(|value| match value.trim().to_lowercase().as_str() {
            "x-high" | "extra-high" | "extra_high" | "max" => "xhigh".to_owned(),
            other => other.to_owned(),
        })
        .filter(|value| ["low", "medium", "high", "xhigh"].contains(&value.as_str()))
        .unwrap_or_else(|| "medium".to_owned());
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
            "This is an external Responses client. Native run_officejs is an intercepted transport, not an OfficeJS executor. For each client tool, call run_officejs with summary, extended_summary, destructive=false, references=[\"CATALOG_NAME\"], and code containing only the serialized JSON arguments object for function tools, or the unchanged raw input text for custom tools. Do not execute OfficeJS or invoke other native tools. Serialize the complete payload, including backslashes and quotes. Do not nest another run_officejs wrapper. Preserve names and schemas exactly. {} Catalog: {}",
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
            // item_reference 与 additional_tools 上游不接受：前者丢弃，
            // 后者的工具已在目录收割阶段并入（与 CPA 插件一致）。
            "item_reference" | "additional_tools" => {}
            _ => {
                input.push(item);
            }
        }
    }
    if tools.get("functions.exec").is_some_and(|tool| tool.custom) {
        input.push(json!({"type":"message","role":"developer","content":[{"type":"input_text","text":
            "Tool routing reminder: APIs documented inside functions.exec (tools.* or mcp__* names) are NOT top-level client tools. Invoke them through outer run_officejs references=[\"functions.exec\"] with JavaScript code such as text(await tools.API_NAME({...}));. Outer references must use a top-level Catalog name. Do not repeat a tool call whose result is already in history."}]}));
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

mod tool_envelope;

// Codex's exec description explicitly declares its nested tools. Only those
// exact object-argument API declarations may be wrapped; this is not an alias
// for arbitrary unknown tools, and the bridge never executes the generated JS.
fn nested_exec_tool<'a>(name: &str, tools: &'a Tools) -> Option<&'a Tool> {
    if name.is_empty() || name.len() > 160
        || !name.bytes().enumerate().all(|(index, c)| c == b'_' || c.is_ascii_alphabetic() || (index > 0 && c.is_ascii_digit()))
        || matches!(name, "constructor" | "prototype" | "__proto__")
    {
        return None;
    }
    let exec = tools.get("functions.exec").filter(|tool| tool.custom)?;
    let description = exec.spec["description"].as_str()?;
    let signature = format!("declare const tools: {{ {name}(args:");
    let (_, remaining) = description.split_once(&signature)?;
    remaining.trim_start().starts_with('{').then_some(exec)
}

fn function_payload(payload: &Value, direct: bool) -> Result<Value> {
    let inner = if direct {
        serde_json::from_str(payload.as_str().ok_or("function code must be text")?)
            .map_err(|_| "invalid function arguments")?
    } else {
        match &payload["arguments"] {
            Value::String(s) => serde_json::from_str(s).map_err(|_| "invalid inner arguments")?,
            v => v.clone(),
        }
    };
    if !inner.is_object() { return Err("function arguments must be an object"); }
    Ok(inner)
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
    let references = args["references"].as_array().filter(|items| !items.is_empty());
    let direct_payload = references.is_some();
    let (tool_key, payload) = if let Some(references) = references {
        if references.len() != 1 {
            return Err("references must select exactly one client tool");
        }
        let name = references[0]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("invalid client tool reference")?;
        if matches!(name, "run_officejs" | "functions.run_officejs") {
            return Err("client tool reference cannot be run_officejs");
        }
        let code = args["code"].as_str().ok_or("tool code must be text")?;
        (name.to_owned(), json!(code))
    } else {
        // Accept the pre-0.8 nested envelope for already persisted histories,
        // while all newly generated requests use the CPA contract above.
        let envelope = tool_envelope::decode(&args["code"])?;
        let name = envelope["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("missing client tool name")?;
        (name.to_owned(), envelope)
    };
    let mut wrapped_input = None;
    let selected = if let Some(tool) = tools.get(&tool_key) {
        Some(tool)
    } else if let Some(exec) = nested_exec_tool(&tool_key, tools) {
        let inner = function_payload(&payload, direct_payload)?;
        // Serialize data as a JSON string, not a JavaScript object literal:
        // quotes/backticks/newlines stay data, and __proto__ stays an own key.
        wrapped_input = Some(format!("text(await tools.{tool_key}(JSON.parse({})));", json!(inner.to_string())));
        eprintln!("bridge nested tool routed {}", json!({"requested":tool_key,"wrapper":"functions.exec"}));
        Some(exec)
    } else { None };
    let tool = selected.ok_or_else(|| {
        // Names only: never log arguments, custom code, headers or credentials.
        eprintln!("bridge undeclared tool {}", json!({
            "requested": tool_key.chars().take(160).collect::<String>(),
            "declared_count": tools.len(),
            "declared_sample": tools.keys().take(16).map(|key| key.chars().take(160).collect::<String>()).collect::<Vec<_>>()
        }));
        "upstream requested an undeclared tool"
    })?;
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
        output["input"] = if let Some(code) = wrapped_input {
            json!(code)
        } else if direct_payload {
            payload
        } else {
            payload["input"].clone()
        };
        if !output["input"].is_string() {
            return Err("custom input must be text");
        }
    } else {
        let inner = function_payload(&payload, direct_payload)?;
        output["arguments"] = json!(inner.to_string());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn exec_source() -> Value {
        json!({"model":"gpt-6-sol-excel","input":"list chats","tools":[{
            "type":"custom","name":"functions.exec",
            "description":"JavaScript orchestration. exec tool declaration:\n```ts\ndeclare const tools: { mcp__codex_app__list_threads(args: { limit?: number; }): Promise<CallToolResult>; };\n```"
        }]})
    }
    #[test]
    fn declared_nested_exec_api_uses_declared_custom_wrapper() {
        let source = exec_source();
        let native = json!({"type":"function_call","id":"fc_nested","call_id":"call_nested","name":"run_officejs",
            "arguments":json!({"references":["mcp__codex_app__list_threads"],"code":"{\"limit\":3}"}).to_string()});
        let restored = restore_call(&native, &tool_catalog(&source)).unwrap();
        assert_eq!(restored["type"], "custom_tool_call");
        assert_eq!(restored["name"], "functions.exec");
        assert_eq!(restored["call_id"], "call_nested");
        assert_eq!(restored["input"], format!("text(await tools.mcp__codex_app__list_threads(JSON.parse({})));", json!("{\"limit\":3}")));
    }
    #[test]
    fn nested_api_fallback_keeps_unknown_tools_and_disabled_exec_rejected() {
        let mut source = exec_source();
        for requested in ["mcp__codex_app__delete_everything", "mcp__codex_app__list_threads;evil()", "constructor"] {
            let native = json!({"name":"run_officejs","call_id":"bad","arguments":json!({"references":[requested],"code":"{}"}).to_string()});
            assert!(restore_call(&native, &tool_catalog(&source)).is_err());
        }
        source["tool_choice"] = json!("none");
        let native = json!({"name":"run_officejs","call_id":"disabled","arguments":json!({"references":["mcp__codex_app__list_threads"],"code":"{}"}).to_string()});
        assert!(restore_call(&native, &tool_catalog(&source)).is_err());
    }
    #[test]
    fn nested_api_requires_explicit_signature_custom_exec_and_object_payload() {
        let native = |code: &str| json!({"name":"run_officejs","call_id":"shape","arguments":json!({"references":["mcp__codex_app__list_threads"],"code":code}).to_string()});
        let mut source = exec_source();
        for code in ["[]", "null", "text(await tools.anything())"] {
            assert!(restore_call(&native(code), &tool_catalog(&source)).is_err());
        }
        source["tools"][0]["description"] = json!("mcp__codex_app__list_threads is mentioned only in prose");
        assert!(restore_call(&native("{}"), &tool_catalog(&source)).is_err());
        source = exec_source();
        source["tools"][0]["type"] = json!("function");
        assert!(restore_call(&native("{}"), &tool_catalog(&source)).is_err());
    }
    #[test]
    fn nested_wrapper_preserves_hostile_strings_and_own_proto_key_as_json_data() {
        let payload = json!({"__proto__":{"polluted":true},"text":"\");globalThis.injected=true;//\n` ${value} \\ 你好"});
        let native = json!({"name":"run_officejs","call_id":"escaping","arguments":json!({"references":["mcp__codex_app__list_threads"],"code":payload.to_string()}).to_string()});
        let out = restore_call(&native, &tool_catalog(&exec_source())).unwrap();
        let code = out["input"].as_str().unwrap();
        let quoted = code.strip_prefix("text(await tools.mcp__codex_app__list_threads(JSON.parse(").unwrap().strip_suffix(")));").unwrap();
        let serialized: String = serde_json::from_str(quoted).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&serialized).unwrap(), payload);
    }
    #[test]
    fn top_level_tool_wins_and_namespaced_exec_retains_identity() {
        let mut source = exec_source();
        let native = json!({"name":"run_officejs","call_id":"identity","arguments":json!({"references":["mcp__codex_app__list_threads"],"code":"{}"}).to_string()});
        source["tools"].as_array_mut().unwrap().push(json!({"type":"function","name":"mcp__codex_app__list_threads"}));
        let out = restore_call(&native, &tool_catalog(&source)).unwrap();
        assert_eq!(out["type"], "function_call");
        assert_eq!(out["name"], "mcp__codex_app__list_threads");
        source = exec_source();
        let mut exec = source["tools"][0].clone();exec["name"] = json!("exec");
        source["tools"] = json!([{"type":"namespace","name":"functions","tools":[exec]}]);
        let out = restore_call(&native, &tool_catalog(&source)).unwrap();
        assert_eq!(out["name"], "exec");
        assert_eq!(out["namespace"], "functions");
    }
    #[test]
    fn nested_tool_stream_emits_one_custom_call_and_preserves_original_history() {
        let source = exec_source();
        let native = json!({"type":"function_call","name":"run_officejs","call_id":"nested_stream","arguments":json!({"references":["mcp__codex_app__list_threads"],"code":"{}"}).to_string()});
        let mut translator = stream::Translator::new(tool_catalog(&source));
        let events = translator.event(json!({"type":"response.output_item.done","output_index":0,"item":native})).unwrap();
        assert_eq!(events.iter().filter(|e| e["type"] == "response.output_item.done").count(), 1);
        let terminal = translator.event(json!({"type":"response.completed","response":{"id":"resp_nested","output":[native]}})).unwrap();
        assert_eq!(terminal.len(), 1);
        assert_eq!(terminal[0]["response"]["output"][0]["type"], "custom_tool_call");
        assert_eq!(translator.originals["nested_stream"], native);
        let mut next = source.clone();
        next["input"] = json!([terminal[0]["response"]["output"][0],{"type":"custom_tool_call_output","call_id":"nested_stream","output":"listed"}]);
        let upstream = prepare(&next, &translator.originals).unwrap();
        assert!(upstream["input"].as_array().unwrap().contains(&native));
        assert!(upstream["input"].as_array().unwrap().last().unwrap()["content"][0]["text"].as_str().unwrap().contains("NOT top-level"));
    }
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
        let prepared = prepare(&s, &BTreeMap::new()).unwrap();
        assert_eq!(prepared["reasoning_effort"], "xhigh");
        s["reasoning"] = json!({"effort":"ultra"});
        let prepared = prepare(&s, &BTreeMap::new()).unwrap();
        assert_eq!(prepared["reasoning_effort"], "medium");
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
        let outer: Value = serde_json::from_str(native["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(outer["references"], json!(["shell"]));
        assert_eq!(outer["code"], json!("{\"cmd\":\"echo 你好\"}"));
        let restored = restore_call(&native, &tool_catalog(&source())).unwrap();
        assert_eq!(restored["arguments"], item["arguments"]);
        assert_eq!(restored["id"], item["id"]);
        assert_eq!(restored["call_id"], item["call_id"]);
    }
    #[test]
    fn cpa_reference_payload_round_trip_supports_custom_tools() {
        let source = json!({
            "model":"gpt-5.6-sol-excel",
            "input":"hello",
            "tools":[{"type":"custom","name":"apply_patch"}]
        });
        let item = json!({
            "type":"custom_tool_call",
            "id":"ctc_original",
            "call_id":"call_custom",
            "name":"apply_patch",
            "input":"*** Begin Patch\n*** End Patch"
        });
        let native = relay_call(&item).unwrap();
        let restored = restore_call(&native, &tool_catalog(&source)).unwrap();
        assert_eq!(restored["type"], "custom_tool_call");
        assert_eq!(restored["input"], item["input"]);
        assert_eq!(restored["call_id"], item["call_id"]);
    }
    #[test]
    fn legacy_empty_references_preserve_declared_tool_payload() {
        let tools = tool_catalog(&source());
        let native = json!({"type":"function_call","call_id":"call_legacy","name":"run_officejs",
            "arguments":json!({"references":[],"code":json!({"name":"shell","arguments":{"cmd":"echo legacy"}}).to_string()}).to_string()});
        let restored = restore_call(&native, &tools).unwrap();
        assert_eq!(restored["name"], "shell");
        assert_eq!(restored["arguments"], json!({"cmd":"echo legacy"}).to_string());
    }
    #[test]
    fn references_do_not_bypass_declared_tool_validation() {
        let tools = tool_catalog(&source());
        for references in [json!([]), json!(["missing"]), json!(["shell","shell"]), json!(["run_officejs"])] {
            let native = json!({"type":"function_call","call_id":"call_bad","name":"run_officejs",
                "arguments":json!({"references":references,"code":"{}"}).to_string()});
            assert!(restore_call(&native, &tools).is_err());
        }
    }
    #[test]
    fn undeclared_tools_and_native_continuation_are_rejected() {
        let mut s = source();
        s["previous_response_id"] = json!("resp_x");
        assert!(prepare(&s, &BTreeMap::new()).is_err());
        let n = json!({"name":"run_officejs","call_id":"a","arguments":json!({"code":"{\"name\":\"unlisted\",\"arguments\":{}}"}).to_string()});
        assert!(restore_call(&n, &tool_catalog(&source())).is_err());
    }
}

#[cfg(test)]
mod request_regression {
    use super::*;
    #[test]
    fn none_disables_tools_and_other_choices_behave_like_auto() {
        let mut source = json!({"model":"gpt-5.6-sol-excel","input":"hello","tools":[{"type":"function","name":"shell"}],"tool_choice":"none"});
        assert!(tool_catalog(&source).is_empty());
        source["tool_choice"] = json!("required");
        source["tools"] = json!([{"type":"function","name":"shell"},{"type":"web_search"}]);
        let catalog = tool_catalog(&source);
        assert_eq!(catalog.len(), 1);
        assert!(catalog.contains_key("shell"));
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
