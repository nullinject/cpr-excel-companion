//! Port of excel-codex-bridge's transport decoder; surrounding text is never evaluated.
use super::{Result, Value};

fn repair_backslashes(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let (mut quoted, mut i) = (false, 0);
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            quoted = !quoted;
        } else if quoted && c == '\\' {
            let next = chars.get(i + 1).copied();
            let valid = match next {
                Some('"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't') => true,
                Some('u') => chars
                    .get(i + 2..i + 6)
                    .is_some_and(|digits| digits.iter().all(char::is_ascii_hexdigit)),
                _ => false,
            };
            if valid {
                out.push(c);
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            out.push('\\');
        }
        out.push(c);
        i += 1;
    }
    out
}

fn object(code: &Value) -> Result<Value> {
    if code.is_object() {
        return Ok(code.clone());
    }
    let text = code.as_str().ok_or("missing tool transport code")?;
    if text.len() > 2 * 1024 * 1024 {
        return Err("tool transport envelope is too large");
    }
    for candidate in [text.to_owned(), repair_backslashes(text)] {
        if let Ok(value) = serde_json::from_str::<Value>(&candidate)
            && value.is_object()
        {
            return Ok(value);
        }
        // Bounded search matches upstream's raw_decode fallback without evaluating wrappers.
        for (index, _) in candidate.match_indices('{').take(64) {
            if let Some(Ok(value)) = serde_json::Deserializer::from_str(&candidate[index..])
                .into_iter::<Value>()
                .next()
                && value.is_object()
            {
                return Ok(value);
            }
        }
    }
    Err("invalid tool transport envelope")
}

fn transport(value: &Value) -> bool {
    matches!(
        value["name"].as_str(),
        Some("run_officejs" | "functions.run_officejs")
    )
}

pub fn decode(code: &Value) -> Result<Value> {
    let mut envelope = object(code)?;
    for _ in 0..2 {
        if !transport(&envelope) {
            return Ok(envelope);
        }
        let args = match &envelope["arguments"] {
            Value::String(text) => {
                serde_json::from_str::<Value>(text).map_err(|_| "invalid nested tool arguments")?
            }
            value => value.clone(),
        };
        envelope = object(&args["code"])?;
    }
    if transport(&envelope) {
        return Err("too many nested tool transport envelopes");
    }
    Ok(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn upstream_wrappers_and_nested_envelopes_preserve_payload() {
        let expected = json!({"name":"shell","arguments":{"cmd":"printf '你好'"}});
        for code in [
            expected.clone(),
            json!(expected.to_string()),
            json!(format!("```json\n{expected}\n```")),
            json!(format!("const tool = {expected};")),
            json!({"name":"functions.run_officejs","arguments":{"code":expected.to_string()}}),
        ] {
            assert_eq!(decode(&code).unwrap(), expected);
        }
    }

    #[test]
    fn invalid_json_escapes_are_preserved_without_evaluation() {
        let code = json!(r#"{"name":"shell","arguments":{"cmd":"rg '\(foo\)' app.py"}}"#);
        let parsed = decode(&code).unwrap();
        assert_eq!(parsed["arguments"]["cmd"], r"rg '\(foo\)' app.py");
        let valid = json!({"name":"shell","arguments":{"cmd":"echo \"quoted\"\nC:\\tmp"}});
        assert_eq!(decode(&json!(valid.to_string())).unwrap(), valid);
        assert!(
            decode(&json!(
                "Excel.run(async context => { await context.sync(); });"
            ))
            .is_err()
        );
        assert!(decode(&json!(r#"{"name":"shell","arguments":{"cmd":"unfinished"#)).is_err());
    }

    #[test]
    fn restored_wrapped_tools_still_require_catalog_membership() {
        let source =
            json!({"tools":[{"type":"function","name":"shell","parameters":{"type":"object"}}]});
        let tools = crate::tool_catalog(&source);
        for name in ["shell", "undeclared"] {
            let code = format!(
                "```json\n{}\n```",
                json!({"name":name,"arguments":{"cmd":"echo ok"}})
            );
            let native = json!({"name":"run_officejs","call_id":"call_1","arguments":json!({"code":code}).to_string()});
            assert_eq!(
                crate::restore_call(&native, &tools).is_ok(),
                name == "shell"
            );
        }
    }
}
