use crate::util::re::{lit, lit_i, Re};
use std::sync::OnceLock;

/// A model call failed. `retry` = the same model may work shortly (rate limit, 5xx, network). `fallback` = this model
/// won't work for us right now (not in plan, unknown model), so try another one. Neither = the request itself is bad.
#[derive(Clone, Debug, Default)]
pub struct ProviderError {
    pub message: String,
    pub status: Option<u16>,
    pub retry: bool,
    pub fallback: bool,
    pub drop_temperature: bool,
    pub drop_max_tokens: bool,
    pub drop_thinking: bool,
    /// The whole provider/plan is out of quota: only another provider can help.
    pub quota: bool,
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl ProviderError {
    pub fn new(message: impl Into<String>, status: Option<u16>) -> ProviderError {
        ProviderError { message: message.into(), status, ..Default::default() }
    }
}

fn quota_re() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| lit(r"usage limit|limit resets|upgrade your plan|insufficient[_ ]quota|exceeded your current quota|insufficient (balance|credit)|out of credits|billing"))
}

fn transient_re() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| {
        lit(
            r"overloaded|temporarily unavailable|try again (later|shortly)|timed? ?out|rate.?limit|too many requests|connection (reset|refused|error|closed)|stream (ended|closed) unexpectedly|bad gateway|service unavailable",
        )
    })
}

pub fn classify(status: Option<u16>, message: &str) -> ProviderError {
    let m = message.to_lowercase();
    let mut e = ProviderError::new(message, status);
    if status == Some(402) || (quota_re().is_match(&m) && !matches!(status, Some(400) | Some(404))) {
        e.fallback = true;
        e.quota = true;
        return e;
    }
    match status {
        Some(429) => {
            e.retry = true;
            e.fallback = true;
            return e;
        }
        None => {
            e.retry = true;
            e.fallback = true;
            return e;
        }
        Some(s) if s >= 500 || s == 408 => {
            e.retry = true;
            e.fallback = true;
            return e;
        }
        Some(403) if m.contains("model_not_in_plan") => {
            e.fallback = true;
            return e;
        }
        Some(401) | Some(403) => return e,
        _ => {}
    }
    if status == Some(404) || m.contains("no available providers") || m.contains("unsupported_model") {
        e.fallback = true;
        return e;
    }
    if transient_re().is_match(&m) {
        e.retry = true;
        e.fallback = true;
        return e;
    }
    if matches!(status, Some(400) | Some(422))
        && lit_i(r"\bthinking\b|enable_thinking|chat_template_kwargs|reasoning_effort|\breasoning\b").is_match(&m)
        && !m.contains("reasoning_content")
    {
        e.drop_thinking = true;
        return e;
    }
    if status == Some(400) && lit(r"max_(completion_)?tokens").is_match(&m) {
        e.drop_max_tokens = true;
        return e;
    }
    if status == Some(400) && lit(r"temperature|top_p").is_match(&m) {
        e.drop_temperature = true;
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kinds() {
        assert!(classify(Some(429), "slow down").retry);
        assert!(classify(Some(402), "pay").quota);
        assert!(classify(Some(400), "bad max_tokens").drop_max_tokens);
        let e = classify(Some(401), "no");
        assert!(!e.retry && !e.fallback);
        assert!(classify(None, "x").retry);
    }
}
