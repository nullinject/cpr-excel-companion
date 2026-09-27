//! 按完整 SSE 事件转换，保持背压；EOF 不伪造成功终态。
use super::{Result, Tools, restore_call};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_EVENT_BYTES: usize = 2 * 1024 * 1024;
#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
}
impl Decoder {
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Value>> {
        self.pending.extend_from_slice(chunk);
        let mut events = Vec::new();
        loop {
            let end = self
                .pending
                .windows(2)
                .position(|w| w == b"\n\n")
                .map(|p| (p, 2));
            let cr = self
                .pending
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|p| (p, 4));
            let boundary = match (end, cr) {
                (Some(a), Some(b)) => Some(if a.0 < b.0 { a } else { b }),
                (a, b) => a.or(b),
            };
            let Some((index, delimiter)) = boundary else {
                break;
            };
            if index > MAX_EVENT_BYTES {
                return Err("upstream SSE event is too large");
            }
            let frame: Vec<u8> = self.pending.drain(..index + delimiter).collect();
            let text = std::str::from_utf8(&frame).map_err(|_| "invalid SSE UTF-8")?;
            let data = text
                .lines()
                .filter_map(|l| {
                    l.strip_prefix("data:")
                        .map(|v| v.strip_prefix(' ').unwrap_or(v))
                })
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            events.push(serde_json::from_str(&data).map_err(|_| "invalid SSE JSON")?);
        }
        if self.pending.len() > MAX_EVENT_BYTES {
            return Err("upstream SSE event is too large");
        }
        Ok(events)
    }
    pub fn finish(&self) -> Result<()> {
        if self.pending.iter().all(u8::is_ascii_whitespace) {
            Ok(())
        } else {
            Err("truncated SSE event")
        }
    }
}

pub struct Translator {
    tools: Tools,
    emitted: BTreeSet<String>,
    pub originals: BTreeMap<String, Value>,
    pub terminal: bool,
    pub completed: Option<Value>,
    sequence: u64,
}
impl Translator {
    pub fn new(tools: Tools) -> Self {
        Self {
            tools,
            emitted: BTreeSet::new(),
            originals: BTreeMap::new(),
            terminal: false,
            completed: None,
            sequence: 0,
        }
    }
    fn tool_events(&mut self, native: &Value, index: Value) -> Result<Vec<Value>> {
        let item = restore_call(native, &self.tools)?;
        let call_id = item["call_id"]
            .as_str()
            .ok_or("missing tool call id")?
            .to_owned();
        self.originals.insert(call_id.clone(), native.clone());
        if !self.emitted.insert(call_id) {
            return Ok(vec![]);
        }
        let custom = item["type"] == "custom_tool_call";
        let field = if custom { "input" } else { "arguments" };
        let base = if custom {
            "response.custom_tool_call_input"
        } else {
            "response.function_call_arguments"
        };
        let mut added = item.clone();
        added[field] = json!("");
        added["status"] = json!("in_progress");
        let mut done =
            json!({"type":format!("{base}.done"),"output_index":index,"item_id":item["id"]});
        done[field] = item[field].clone();
        Ok(vec![
            json!({"type":"response.output_item.added","output_index":index,"item":added}),
            json!({"type":format!("{base}.delta"),"output_index":index,"item_id":item["id"],"delta":item[field]}),
            done,
            json!({"type":"response.output_item.done","output_index":index,"item":item}),
        ])
    }
    pub fn event(&mut self, mut event: Value) -> Result<Vec<Value>> {
        if self.terminal {
            return Ok(vec![]);
        }
        let kind = event["type"]
            .as_str()
            .ok_or("missing SSE event type")?
            .to_owned();
        let mut out = match kind.as_str() {
            "error" | "response.failed" | "response.incomplete" => {
                self.terminal = true;
                // 不转发可能回显提示词或凭据的原始错误对象。
                vec![
                    json!({"type":"response.failed","response":{"id":event.pointer("/response/id").and_then(Value::as_str).unwrap_or("resp_excel_failed"),"object":"response","status":"failed","output":[],"error":{"code":"excel_upstream_failed","message":"Excel upstream did not complete the response"}}}),
                ]
            }
            "response.function_call_arguments.delta"
            | "response.function_call_arguments.done"
            | "response.custom_tool_call_input.delta"
            | "response.custom_tool_call_input.done" => vec![],
            "response.output_item.added"
                if matches!(
                    event["item"]["type"].as_str(),
                    Some("function_call" | "custom_tool_call")
                ) =>
            {
                vec![]
            }
            "response.output_item.done"
                if matches!(
                    event["item"]["type"].as_str(),
                    Some("function_call" | "custom_tool_call")
                ) =>
            {
                self.tool_events(&event["item"], event["output_index"].clone())?
            }
            "response.completed" => {
                let items = event
                    .pointer("/response/output")
                    .and_then(Value::as_array)
                    .ok_or("completed response has no output")?
                    .clone();
                let mut converted = Vec::new();
                let mut out = Vec::new();
                for (index, item) in items.into_iter().enumerate() {
                    if matches!(
                        item["type"].as_str(),
                        Some("function_call" | "custom_tool_call")
                    ) {
                        out.extend(self.tool_events(&item, json!(index))?);
                        converted.push(restore_call(&item, &self.tools)?);
                    } else {
                        converted.push(item);
                    }
                }
                event["response"]["output"] = json!(converted);
                self.completed = Some(event["response"].clone());
                self.terminal = true;
                out.push(event);
                out
            }
            _ => vec![event],
        };
        for event in &mut out {
            event["sequence_number"] = json!(self.sequence);
            self.sequence += 1;
        }
        Ok(out)
    }
}

pub fn encode(event: &Value) -> Vec<u8> {
    // SSE 注释：桥接保活心跳借道输出，所有合规解析器都忽略注释行。
    if event["type"] == "bridge.comment" {
        return format!(": {}\n\n", event["text"].as_str().unwrap_or("keepalive")).into_bytes();
    }
    format!(
        "event: {}\ndata: {}\n\n",
        event["type"].as_str().unwrap_or("error"),
        event
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decodes_split_unicode_and_crlf() {
        let data="event: response.output_text.delta\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"你好\"}\r\n\r\n".as_bytes();
        let mut decoder = Decoder::default();
        let mut out = vec![];
        for b in data {
            out.extend(decoder.push(&[*b]).unwrap());
        }
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["delta"], "你好");
        assert!(decoder.finish().is_ok());
    }
    #[test]
    fn truncated_stream_does_not_become_success() {
        let mut d = Decoder::default();
        d.push(b"data: {\"type\":").unwrap();
        assert!(d.finish().is_err());
    }
    #[test]
    fn error_has_one_terminal_and_does_not_echo_secrets() {
        let mut t = Translator::new(Tools::new());
        let out = t
            .event(json!({"type":"error","message":"private prompt secret"}))
            .unwrap();
        assert_eq!(out.len(), 1);
        assert!(!json!(out).to_string().contains("private prompt"));
        assert!(
            t.event(json!({"type":"response.failed"}))
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn tool_call_is_emitted_once_and_preserves_original() {
        let source = json!({"tools":[{"type":"custom","name":"apply_patch"}]});
        let mut t = Translator::new(super::super::tool_catalog(&source));
        let native = json!({"type":"function_call","id":"fc_native","call_id":"call_1","name":"run_officejs","arguments":json!({"code":json!({"name":"apply_patch","input":"*** Begin Patch\n*** End Patch"}).to_string()}).to_string()});
        assert!(
            t.event(json!({"type":"response.output_item.added","output_index":0,"item":native}))
                .unwrap()
                .is_empty()
        );
        let out = t
            .event(json!({"type":"response.output_item.done","output_index":0,"item":native}))
            .unwrap();
        assert_eq!(out.len(), 4);
        assert_eq!(out[3]["item"]["type"], "custom_tool_call");
        let end=t.event(json!({"type":"response.completed","response":{"id":"resp_1","output":[native],"usage":{"input_tokens":10}}})).unwrap();
        assert_eq!(end.len(), 1);
        assert_eq!(t.originals["call_1"]["id"], "fc_native");
        assert_eq!(t.completed.as_ref().unwrap()["usage"]["input_tokens"], 10);
    }
}
