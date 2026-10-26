use crate::util::re::lit;
use serde_json::{json, Map, Value};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TokenUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub cached_tokens: i64,
    /// Tokens the provider stored for later reuse (billed at a premium by some).
    pub cache_write_tokens: i64,
}

impl TokenUsage {
    pub fn add(&mut self, o: &TokenUsage) {
        self.prompt_tokens += o.prompt_tokens;
        self.completion_tokens += o.completion_tokens;
        self.total_tokens += o.total_tokens;
        self.cached_tokens += o.cached_tokens;
        self.cache_write_tokens += o.cache_write_tokens;
    }

    pub fn to_value(&self) -> Value {
        json!({"prompt_tokens": self.prompt_tokens, "completion_tokens": self.completion_tokens, "total_tokens": self.total_tokens,
               "cached_tokens": self.cached_tokens, "cache_write_tokens": self.cache_write_tokens})
    }

    pub fn from_value(v: &Value) -> TokenUsage {
        let g = |k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0);
        TokenUsage {
            prompt_tokens: g("prompt_tokens"),
            completion_tokens: g("completion_tokens"),
            total_tokens: g("total_tokens"),
            cached_tokens: g("cached_tokens"),
            cache_write_tokens: g("cache_write_tokens"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: Map<String, Value>,
}

#[derive(Clone, Debug)]
pub enum StreamEvent {
    TextDelta(String),
    ToolCallStart { call_id: String, name: String },
    ToolCallDelta { call_id: String, arguments_delta: String },
    ToolCallComplete(ToolCall),
    MessageComplete { finish_reason: Option<String>, usage: Option<TokenUsage>, reasoning: Option<String>, text: Option<String> },
    Error(String),
    Notice(String),
}

/// The model's argument string as an object. Open models often send almost-JSON, so a few safe repairs run before
/// giving up: raw newlines inside strings, a regex backslash left unescaped (`\d+` sent with one backslash), a
/// ```json fence, or the whole object encoded twice. Anything else comes back as raw_arguments and the model is
/// asked to re-send.
pub fn parse_tool_call_arguments(s: &str) -> Map<String, Value> {
    if s.trim().is_empty() {
        return Map::new();
    }
    let mut text = s.trim().to_string();
    if text.starts_with("```") {
        text = lit(r"^```\w*\s*").replace_all(&text, "").to_string();
        text = lit(r"\s*```$").replace_all(&text, "").to_string();
    }
    let lone = fix_lone_backslashes(&text);
    for attempt in [text.clone(), lone] {
        let attempt = escape_raw_controls(&attempt);
        let Ok(mut value) = serde_json::from_str::<Value>(&attempt) else { continue };
        if let Value::String(inner) = &value {
            // '"{\"path\": \"a\"}"': the object was serialised twice
            if let Ok(v) = serde_json::from_str::<Value>(&escape_raw_controls(inner)) {
                value = v;
            }
        }
        if let Value::Object(m) = value {
            return m;
        }
    }
    let mut m = Map::new();
    m.insert("raw_arguments".into(), Value::String(s.to_string()));
    m
}

/// A lone backslash before a character that is not a JSON escape is doubled (pairs are read left to right).
fn fix_lone_backslashes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some(n) if "\"\\/bfnrtu".contains(n) => {
                out.push('\\');
                out.push(n);
            }
            Some(n) => {
                out.push_str("\\\\");
                out.push(n);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Python's `json.loads(strict=False)` allows raw control characters inside strings; serde does not. Escape them.
fn escape_raw_controls(text: &str) -> String {
    let (mut out, mut in_str, mut esc) = (String::with_capacity(text.len()), false, false);
    for c in text.chars() {
        if in_str {
            if esc {
                esc = false;
                out.push(c);
            } else if c == '\\' {
                esc = true;
                out.push(c);
            } else if c == '"' {
                in_str = false;
                out.push(c);
            } else if c == '\n' {
                out.push_str("\\n");
            } else if c == '\r' {
                out.push_str("\\r");
            } else if c == '\t' {
                out.push_str("\\t");
            } else if (c as u32) < 0x20 {
                out.push_str(&format!("\\u{:04x}", c as u32));
            } else {
                out.push(c);
            }
        } else {
            if c == '"' {
                in_str = true;
            }
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repairs() {
        assert_eq!(parse_tool_call_arguments(r#"{"a":1}"#)["a"], 1);
        assert_eq!(parse_tool_call_arguments("{\"a\":\"x\ny\"}")["a"], "x\ny");
        assert_eq!(parse_tool_call_arguments(r#"{"p":"\d+"}"#)["p"], "\\d+");
        assert_eq!(parse_tool_call_arguments("```json\n{\"a\":2}\n```")["a"], 2);
        assert_eq!(parse_tool_call_arguments(r#""{\"a\":3}""#)["a"], 3);
        assert!(parse_tool_call_arguments("nope").contains_key("raw_arguments"));
        assert!(parse_tool_call_arguments("").is_empty());
    }
}
