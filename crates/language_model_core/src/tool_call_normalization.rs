use serde_json::Value;
use std::sync::Arc;

use crate::util::{fix_streamed_json, parse_tool_arguments};

/// A tool call recovered from model prose (markdown, XML, pseudo-JSON).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedToolCall {
    pub name: Arc<str>,
    pub input: Value,
    pub raw_input: String,
}

/// Parse tool arguments, applying JSON repair for truncated or malformed payloads.
pub fn normalize_tool_arguments(arguments: &str) -> Result<Value, String> {
    parse_tool_arguments(arguments).map_err(|error| error.to_string())
}

/// Parse arguments with repair fallback when strict parsing fails.
pub fn normalize_tool_arguments_lenient(arguments: &str) -> Result<Value, String> {
    normalize_tool_arguments(arguments).or_else(|_| {
        let fixed = fix_streamed_json(arguments);
        parse_tool_arguments(&fixed).map_err(|error| error.to_string())
    })
}

/// Scan assistant text for embedded tool calls when the model did not use native tool APIs.
pub fn extract_tool_calls_from_text(text: &str, known_tools: &[&str]) -> Vec<ExtractedToolCall> {
    let mut results = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for json_blob in extract_json_candidates(text) {
        if let Some(call) = parse_tool_call_value(&json_blob, known_tools) {
            let key = format!("{}:{}", call.name, call.raw_input);
            if seen.insert(key) {
                results.push(call);
            }
        }
    }

    for call in extract_function_parameter_calls(text, known_tools) {
        let key = format!("{}:{}", call.name, call.raw_input);
        if seen.insert(key) {
            results.push(call);
        }
    }

    for call in extract_xml_tool_calls(text, known_tools) {
        let key = format!("{}:{}", call.name, call.raw_input);
        if seen.insert(key) {
            results.push(call);
        }
    }

    results
}

/// Qwen / VLM models often emit `<function=read_file><parameter=path>...</parameter></function>`.
fn extract_function_parameter_calls(text: &str, known_tools: &[&str]) -> Vec<ExtractedToolCall> {
    let mut results = Vec::new();
    let mut search_from = 0;

    while let Some(open_ix) = text[search_from..].find("<function=") {
        let abs_open = search_from + open_ix;
        let after_open = &text[abs_open + "<function=".len()..];
        let Some(name_end) = after_open.find('>') else {
            break;
        };
        let name = after_open[..name_end].trim();
        if name.is_empty() {
            search_from = abs_open + 1;
            continue;
        }

        let body_start = abs_open + "<function=".len() + name_end + 1;
        let close_tag = "</function>";
        let Some(close_ix) = text[body_start..].find(close_tag) else {
            break;
        };
        let body = &text[body_start..body_start + close_ix];

        if !known_tools.is_empty() && !known_tools.iter().any(|tool| *tool == name) {
            search_from = body_start + close_ix + close_tag.len();
            continue;
        }

        let mut input = serde_json::Map::new();
        let mut param_search = 0;
        while let Some(param_open) = body[param_search..].find("<parameter=") {
            let param_abs = param_search + param_open;
            let after_param = &body[param_abs + "<parameter=".len()..];
            let Some(param_name_end) = after_param.find('>') else {
                break;
            };
            let param_name = after_param[..param_name_end].trim();
            let content_start = param_abs + "<parameter=".len() + param_name_end + 1;
            let param_close = "</parameter>";
            let Some(param_close_ix) = body[content_start..].find(param_close) else {
                break;
            };
            let value = body[content_start..content_start + param_close_ix].trim();
            input.insert(param_name.to_string(), Value::String(value.to_string()));
            param_search = content_start + param_close_ix + param_close.len();
        }

        let input = Value::Object(input);
        let raw_input = input.to_string();
        results.push(ExtractedToolCall {
            name: Arc::from(name),
            input,
            raw_input,
        });

        search_from = body_start + close_ix + close_tag.len();
    }

    results
}

/// Recover a tool call when the model put XML in the native `name` field.
pub fn repair_malformed_tool_use(name: &str, raw_arguments: &str) -> Option<ExtractedToolCall> {
    let combined = format!("{name}{raw_arguments}");
    extract_function_parameter_calls(&combined, &[])
        .into_iter()
        .next()
        .or_else(|| {
            extract_tool_calls_from_text(&combined, &[])
                .into_iter()
                .next()
        })
}

fn parse_tool_call_value(json: &str, known_tools: &[&str]) -> Option<ExtractedToolCall> {
    let value: Value = serde_json::from_str(json)
        .ok()
        .or_else(|| serde_json::from_str(&fix_streamed_json(json)).ok())?;

    if let Some(call) = tool_call_from_object(&value, known_tools) {
        return Some(call);
    }

    if let Some(function) = value.get("function") {
        return tool_call_from_object(function, known_tools);
    }

    None
}

fn tool_call_from_object(value: &Value, known_tools: &[&str]) -> Option<ExtractedToolCall> {
    let name = value
        .get("name")
        .or_else(|| value.get("tool"))
        .and_then(Value::as_str)?;

    if !known_tools.is_empty() && !known_tools.iter().any(|tool| *tool == name) {
        return None;
    }

    let arguments = value
        .get("arguments")
        .or_else(|| value.get("parameters"))
        .or_else(|| value.get("input"))
        .or_else(|| value.get("args"));

    let (input, raw_input) = match arguments {
        Some(Value::String(raw)) => {
            let parsed = normalize_tool_arguments_lenient(raw).unwrap_or_else(|_| Value::Object(Default::default()));
            (parsed, raw.clone())
        }
        Some(other) => {
            let raw = other.to_string();
            (other.clone(), raw)
        }
        None => (Value::Object(Default::default()), "{}".into()),
    };

    Some(ExtractedToolCall {
        name: Arc::from(name),
        input,
        raw_input,
    })
}

fn extract_json_candidates(text: &str) -> Vec<String> {
    let mut candidates = Vec::new();

    for segment in text.split("```") {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        let json_body = trimmed
            .strip_prefix("json")
            .unwrap_or(trimmed)
            .trim();
        if json_body.starts_with('{') || json_body.starts_with('[') {
            candidates.push(json_body.to_string());
        }
    }

    candidates.extend(find_balanced_json_objects(text));
    candidates
}

fn find_balanced_json_objects(text: &str) -> Vec<String> {
    let mut results = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'{' {
            if let Some(end) = find_json_object_end(text, index) {
                results.push(text[index..=end].to_string());
                index = end + 1;
                continue;
            }
        }
        index += 1;
    }

    results
}

fn find_json_object_end(text: &str, start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escape = false;
    let bytes = text.as_bytes();

    for (offset, &byte) in bytes[start..].iter().enumerate() {
        if in_string {
            if escape {
                escape = false;
            } else if byte == b'\\' {
                escape = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }

        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(start + offset);
                }
            }
            _ => {}
        }
    }

    None
}

fn extract_xml_tool_calls(text: &str, known_tools: &[&str]) -> Vec<ExtractedToolCall> {
    let mut results = Vec::new();

    for (tool_name, body) in extract_xml_blocks(text, "tool_call")
        .into_iter()
        .chain(extract_xml_blocks(text, "function_call"))
    {
        if tool_name.is_empty() {
            if let Some(arguments) = body.get("arguments") {
                if let Some(call) = parse_tool_call_value(arguments, known_tools) {
                    results.push(call);
                    continue;
                }
            }
        }

        let name = if tool_name.is_empty() {
            body.get("name")
                .or_else(|| body.get("tool"))
                .cloned()
                .unwrap_or_default()
        } else {
            tool_name
        };

        if name.is_empty() {
            continue;
        }

        if !known_tools.is_empty() && !known_tools.iter().any(|tool| *tool == name) {
            continue;
        }

        let raw_input = body
            .get("arguments")
            .or_else(|| body.get("parameters"))
            .cloned()
            .unwrap_or_else(|| "{}".into());

        let input = normalize_tool_arguments_lenient(&raw_input)
            .unwrap_or_else(|_| Value::Object(Default::default()));

        results.push(ExtractedToolCall {
            name: Arc::from(name),
            input,
            raw_input,
        });
    }

    results
}

/// Returns `(name attribute or empty, inner text)` for simple XML-like blocks.
fn extract_xml_blocks(text: &str, tag: &str) -> Vec<(String, std::collections::HashMap<String, String>)> {
    let mut results = Vec::new();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");

    let mut search_from = 0;
    while let Some(open_ix) = text[search_from..].find(&open) {
        let abs_open = search_from + open_ix;
        let after_open = &text[abs_open + open.len()..];
        let Some(close_gt) = after_open.find('>') else {
            break;
        };
        let header = &after_open[..close_gt];
        let name = parse_xml_name_attr(header);
        let content_start = abs_open + open.len() + close_gt + 1;
        let Some(close_ix) = text[content_start..].find(&close) else {
            break;
        };
        let inner = &text[content_start..content_start + close_ix];
        let mut body = std::collections::HashMap::new();
        if inner.trim_start().starts_with('{') {
            body.insert("arguments".into(), inner.trim().to_string());
        } else {
            body.insert("name".into(), inner.trim().to_string());
        }
        results.push((name, body));
        search_from = content_start + close_ix + close.len();
    }

    results
}

fn parse_xml_name_attr(header: &str) -> String {
    for part in header.split_whitespace() {
        if let Some(value) = part.strip_prefix("name=\"").and_then(|v| v.strip_suffix('"')) {
            return value.to_string();
        }
        if let Some(value) = part.strip_prefix("name='").and_then(|v| v.strip_suffix('\'')) {
            return value.to_string();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_repairs_truncated_json() {
        let args = r#"{"path": "Cargo.toml""#;
        let parsed = normalize_tool_arguments_lenient(args).expect("should repair");
        assert_eq!(parsed["path"], "Cargo.toml");
    }

    #[test]
    fn extract_from_markdown_json_block() {
        let text = indoc::indoc! {r#"
            I'll read the file.

            ```json
            {"name": "read_file", "arguments": {"path": "Cargo.toml"}}
            ```
        "#};

        let calls = extract_tool_calls_from_text(text, &["read_file"]);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name.as_ref(), "read_file");
        assert_eq!(calls[0].input["path"], "Cargo.toml");
    }

    #[test]
    fn extract_ignores_unknown_tools_when_filter_set() {
        let text = r#"{"name": "unknown_tool", "arguments": {}}"#;
        assert!(extract_tool_calls_from_text(text, &["read_file"]).is_empty());
    }

    #[test]
    fn extract_from_xml_tool_call() {
        let text = r#"<tool_call name="terminal">{"command": "git status"}</tool_call>"#;
        let calls = extract_tool_calls_from_text(text, &["terminal"]);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].input["command"], "git status");
    }

    #[test]
    fn extract_from_xml_tool_call_with_json_name_and_arguments() {
        let text = r#"<tool_call>
{"name": "read_file", "arguments": {"path": "Cargo.toml"}}
</tool_call>"#;
        let calls = extract_tool_calls_from_text(text, &["read_file"]);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name.as_ref(), "read_file");
        assert_eq!(calls[0].input["path"], "Cargo.toml");
    }

    #[test]
    fn extract_from_function_parameter_format() {
        let text = indoc::indoc! {r#"
            <function=read_file>
            <parameter=path>
            Cargo.toml
            </parameter>
            </function>
        "#};
        let calls = extract_tool_calls_from_text(text, &["read_file"]);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name.as_ref(), "read_file");
        assert_eq!(calls[0].input["path"], "Cargo.toml");
    }

    #[test]
    fn repair_full_xml_tool_name() {
        let name = "<function=read_file>\n<parameter=path>\nCargo.toml\n</parameter>\n</function>";
        let repaired = repair_malformed_tool_use(name, "").expect("should repair");
        assert_eq!(repaired.name.as_ref(), "read_file");
        assert_eq!(repaired.input["path"], "Cargo.toml");
    }
}
