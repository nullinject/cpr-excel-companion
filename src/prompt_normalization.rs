//! Optional task-clarification preface. Original text and attachments stay intact.
use serde_json::{Value, json};
pub const PREFACE: &str = "请先依据原始请求明确任务目标、操作对象、已知输入、约束和期望输出，再执行任务。只使用用户实际提供的信息；不要扩大原意、虚构权限或授权。必要信息缺失时先询问。下面保留用户原始请求：\n\n";

/// Only the final user turn is eligible. Tool/assistant/developer content is untouched.
/// Run after recording the original input and attachment preparation, before prepare().
pub fn apply(source: &mut Value) -> bool {
    let Some(input) = source.get_mut("input") else { return false; };
    if let Some(text) = input.as_str() {
        if text.is_empty() || text.starts_with(PREFACE) { return false; }
        *input = json!(format!("{PREFACE}{text}"));
        return true;
    }
    let Some(message) = input.as_array_mut().and_then(|items| items.last_mut()) else { return false; };
    if message["role"] != "user" { return false; }
    let Some(content) = message.get_mut("content") else { return false; };
    if let Some(text) = content.as_str() {
        if text.is_empty() || text.starts_with(PREFACE) { return false; }
        *content = json!(format!("{PREFACE}{text}"));
        return true;
    }
    let Some(parts) = content.as_array_mut() else { return false; };
    if parts.iter().any(|part| {
        part["type"] == "input_text"
            && part["text"].as_str().is_some_and(|text| text.starts_with(PREFACE))
    }) { return false; }
    if !parts.iter().any(|part|part["type"] == "input_text" && part["text"].as_str().is_some_and(|s|!s.is_empty())) { return false; }
    parts.insert(0, json!({"type":"input_text","text":PREFACE}));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn string_preserves_original_verbatim_and_is_idempotent() {
        let original="  原始请求\n保持原始约束\n";
        let mut source=json!({"input":original,"instructions":"unchanged","metadata":{"x":1}});
        assert!(apply(&mut source));
        assert_eq!(source["input"],format!("{PREFACE}{original}"));
        assert_eq!(source["instructions"],"unchanged");
        assert!(!apply(&mut source));
    }
    #[test]
    fn preserves_history_and_attachments_without_substituting_intent() {
        let input=json!([
            {"role":"developer","content":"instructions"},
            {"role":"user","content":"prior user"},
            {"role":"assistant","content":"prior refusal"},
            {"role":"user","content":[{"type":"input_text","text":"original request"},{"type":"input_image","image_url":"fixture-image"},{"type":"input_file","file_id":"fixture-file"}]}
        ]);
        let mut source=json!({"input":input});assert!(apply(&mut source));
        for index in 0..3 {assert_eq!(source["input"][index],input[index]);}
        let parts=source["input"][3]["content"].as_array().unwrap();
        assert_eq!(&parts[1..],input[3]["content"].as_array().unwrap());
        assert!(!apply(&mut source));
    }
    #[test]
    fn never_normalizes_tools_non_user_messages_or_attachment_only_turns() {
        for last in [json!({"type":"function_call_output","output":"private tool output"}),json!({"role":"assistant","content":"assistant"}),json!({"role":"developer","content":"developer"}),json!({"role":"user","content":[{"type":"input_image","image_url":"only image"}]})] {
            let mut source=json!({"input":[{"role":"user","content":"prior user"},last]});
            let original=source.clone();assert!(!apply(&mut source));assert_eq!(source,original);
        }
    }
    #[test]
    fn does_not_stack_preface_when_a_prior_transform_joined_the_text_part() {
        let mut source = json!({"input":[{"role":"user","content":[
            {"type":"input_text","text":format!("{PREFACE}original")}
        ]}]});
        let original = source.clone();
        assert!(!apply(&mut source));
        assert_eq!(source, original);
    }
}
