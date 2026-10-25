//! Ported from tests/test_ask_user.py: ask_user shows each question with its context and option effects.
use crate::config::ConfigHandle;
use crate::tools::builtin::ask_user::AskUserTool;
use crate::tools::{Tool, ToolInvocation};
use crate::ui::prompt::{set_asker, Asker};
use futures::future::BoxFuture;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

/// A pretend person: picks the second option, then "something else", and types a free-text answer.
struct Fake {
    picks: Mutex<Vec<usize>>,
    shown: Arc<Mutex<Vec<(String, Vec<String>)>>>,
}

impl Asker for Fake {
    fn ask_choice(&self, q: String, opts: Vec<String>) -> BoxFuture<'static, usize> {
        self.shown.lock().unwrap().push((q, opts));
        let n = self.picks.lock().unwrap().remove(0);
        Box::pin(async move { n })
    }
    fn read_line(&self, _prompt: String) -> BoxFuture<'static, String> {
        Box::pin(async { "only CANCELLED".to_string() })
    }
    fn say(&self, _line: String) {}
}

#[tokio::test]
async fn ask_user_returns_the_picked_label_and_free_text() {
    let tmp = tempfile::tempdir().unwrap();
    let shown = Arc::new(Mutex::new(vec![]));
    set_asker(Some(Arc::new(Fake { picks: Mutex::new(vec![1, 2]), shown: shown.clone() })));
    let qs: Vec<Value> = (1..=2)
        .map(|n| {
            json!({"header": "Status", "question": format!("Q{n}?"), "context": "Found 2 filters in jobs.py.",
                   "options": [{"label": "a", "description": "keeps it"}, {"label": "b", "description": "why"}]})
        })
        .collect();
    let tool = AskUserTool::new(ConfigHandle::new(super::cfg(tmp.path(), json!({}))));
    let res = tool.execute(&ToolInvocation { params: json!({"questions": qs}), cwd: tmp.path().into(), epoch: 0, call_id: String::new(), progress: None }).await;
    set_asker(None);
    assert!(res.output.contains("Q1? -> b") && res.output.contains("Q2? -> only CANCELLED"), "{}", res.output);
    // the user sees the topic, which question this is, the scope, and what each option does
    let shown = shown.lock().unwrap();
    assert_eq!(shown[0].0, "Status (1/2): Q1?\nFound 2 filters in jobs.py.");
    assert_eq!(&shown[0].1[..2], ["a — keeps it", "b — why"]);
}

#[test]
fn ask_user_requires_context_and_option_effects() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = AskUserTool::new(ConfigHandle::new(super::cfg(tmp.path(), json!({}))));
    let bad = json!({"questions": [{"header": "X", "question": "Q?", "options": [{"label": "a"}, {"label": "b"}]}]});
    assert!(tool.validate_params(&bad).is_err());
}
