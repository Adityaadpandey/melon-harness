use crate::config::ConfigHandle;
use crate::tools::{schema_of, Tool, ToolInvocation, ToolKind, ToolResult};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::any::Any;

const OTHER: &str = "Something else (type it)";

pub struct AskUserTool {
    config: ConfigHandle,
}
impl AskUserTool {
    pub fn new(config: ConfigHandle) -> Self {
        AskUserTool { config }
    }
}

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }
    fn description(&self) -> String {
        "Ask the user to choose when an unknown changes the result and the repo cannot answer it. Use this instead of writing options and letters in your reply: the user answers with one key. Never use it for permission to run or edit, or for things you can look up.\n\
The user has not seen your tool calls, so every question must stand on its own:\n\
- one decision per question, in plain words; explain any internal name, file or term you mention\n\
- context says what you found and the exact scope the answer controls (e.g. 'I found 3 callers of parse_date: api.py, jobs.py, cli.py. This decides which of them switch to UTC.')\n\
- 2-4 options that really differ, recommended first, each with a description of its concrete effect; no 'other' option (it is added)\n\
How to word it:\n\
- talk to the user as 'you', in short everyday sentences, the way a colleague would ask at their desk\n\
- ask about the outcome they want, not your internals: 'Should old links keep working?' beats 'Add a redirect shim in router.py?'\n\
- name both sides of the choice; no yes/no on a vague verb ('Should I handle this?'), no double negatives, no 'etc.' or 'and so on'\n\
- keep it neutral: the recommendation goes on the option, not inside the question\n\
- options read as finished answers to the question ('Keep the old format'), not as tasks for you ('Refactor parser')\n\
Bad: 'How to handle the legacy config edge case?' / options 'Shim', 'Migrate', 'Ignore'.\n\
Good: header 'Old configs', question 'What should happen to config files written before v2?', context 'v2 renamed `port` to `listen`; 14 example configs in docs/ still use `port`. This decides whether loader.py accepts both names.', options 'Accept both names (Recommended)': old files keep working, loader.py gets a 3-line fallback / 'Only the new name': old files fail with a clear error telling the user to rename `port`.".into()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn is_mutating(&self, _p: &Value) -> bool {
        false
    }
    fn schema(&self) -> Value {
        let option = json!({"type": "object", "properties": {
            "label": {"type": "string", "description": "Short choice, 1-5 words. Add '(Recommended)' to the one you would pick"},
            "description": {"type": "string", "description": "One plain sentence on what concretely happens if they pick it, e.g. 'Only users.py changes; billing keeps the old format'"}}, "required": ["label", "description"]});
        let question = json!({"type": "object", "properties": {
            "header": {"type": "string", "description": "Topic of the decision in 1-3 words, e.g. 'Date format' or 'Which tables'"},
            "question": {"type": "string", "description": "One decision, asked in plain words a newcomer to this repo would understand, ending with a question mark"},
            "context": {"type": "string", "description": "1-2 sentences: what you found that makes this a question, and exactly what the answer will change (which files, behaviour or part of the task). Name things, never 'it' or 'this'"},
            "options": {"type": "array", "minItems": 2, "maxItems": 4, "items": option}}, "required": ["header", "question", "context", "options"]});
        schema_of(json!({"questions": {"type": "array", "minItems": 1, "maxItems": 4, "description": "Up to 4 independent questions asked one after another", "items": question}}), &["questions"])
    }
    async fn execute(&self, inv: &ToolInvocation) -> ToolResult {
        let Some(asker) = crate::ui::prompt::asker() else {
            return ToolResult::error("No user is available to answer. Pick the most reasonable option yourself and say which one you assumed.");
        };
        if self.config.get().model_role != "default" {
            return ToolResult::error("No user is available to answer. Pick the most reasonable option yourself and say which one you assumed.");
        }
        let questions = inv.params["questions"].as_array().cloned().unwrap_or_default();
        let mut answers = vec![];
        for (n, q) in questions.iter().enumerate() {
            let step = if questions.len() > 1 { format!(" ({}/{})", n + 1, questions.len()) } else { String::new() };
            // first line is the headline, the lines after it are shown dimmed underneath as context
            let shown = format!("{}{step}: {}\n{}", q["header"].as_str().unwrap_or(""), q["question"].as_str().unwrap_or(""), q["context"].as_str().unwrap_or(""));
            let opts_src = q["options"].as_array().cloned().unwrap_or_default();
            let mut opts: Vec<String> = opts_src.iter().map(|o| format!("{} — {}", o["label"].as_str().unwrap_or(""), o["description"].as_str().unwrap_or(""))).collect();
            opts.push(OTHER.into());
            let i = asker.ask_choice(shown, opts).await;
            let answer = if i >= opts_src.len() {
                let t = asker.read_line("Your answer (Enter to skip): ".into()).await;
                if t.trim().is_empty() { "(skipped)".to_string() } else { t }
            } else {
                opts_src[i]["label"].as_str().unwrap_or("").to_string()
            };
            asker.say(format!("  ? {} → {answer}", q["question"].as_str().unwrap_or("")));
            answers.push(format!("{} -> {answer}", q["question"].as_str().unwrap_or("")));
        }
        ToolResult::ok(format!("The user answered:\n{}", answers.join("\n")))
    }
}
