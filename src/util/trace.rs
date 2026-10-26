//! Structured event log. One JSON object per line, one file per session. Everything the harness decides
//! (model picked, fallback, retry, tool run) goes through here so a bad run can be replayed from the file.
use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct Tracer {
    pub session_id: Mutex<String>,
    pub events: Mutex<Vec<Value>>,
    pub path: Mutex<Option<PathBuf>>,
}

impl Tracer {
    pub fn new(session_id: &str, dir: Option<PathBuf>) -> Tracer {
        let path = dir.and_then(|d| std::fs::create_dir_all(&d).ok().map(|_| d.join(format!("{session_id}.jsonl"))));
        Tracer { session_id: Mutex::new(session_id.to_string()), events: Mutex::new(vec![]), path: Mutex::new(path) }
    }

    /// The same session under a new id (a resumed session): the trace file follows it.
    pub fn rename(&self, id: &str) {
        *self.session_id.lock().unwrap() = id.to_string();
        let mut p = self.path.lock().unwrap();
        if let Some(path) = p.as_mut() {
            *path = path.with_file_name(format!("{id}.jsonl"));
        }
    }

    fn scrub(v: Value) -> Value {
        let secret = std::env::var("API_KEY").ok().filter(|s| !s.is_empty());
        match v {
            Value::String(mut s) => {
                if let Some(k) = &secret {
                    if s.contains(k.as_str()) {
                        s = s.replace(k.as_str(), "***");
                    }
                }
                if s.chars().count() > 2000 {
                    s = s.chars().take(2000).collect::<String>() + "...[clipped]";
                }
                Value::String(s)
            }
            Value::Object(m) => Value::Object(m.into_iter().map(|(k, v)| (k, Self::scrub(v))).collect()),
            Value::Array(a) => Value::Array(a.into_iter().map(Self::scrub).collect()),
            other => other,
        }
    }

    pub fn emit(&self, event: &str, fields: Value) {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let mut rec = Map::new();
        rec.insert("t".into(), json!((now * 1000.0).round() / 1000.0));
        rec.insert("session".into(), json!(self.session_id.lock().unwrap().clone()));
        rec.insert("event".into(), json!(event));
        if let Value::Object(m) = Self::scrub(fields) {
            rec.extend(m);
        }
        let rec = Value::Object(rec);
        if let Some(path) = self.path.lock().unwrap().as_ref() {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "{rec}");
            }
        }
        self.events.lock().unwrap().push(rec);
    }

    pub fn count(&self, event: &str) -> usize {
        self.events.lock().unwrap().iter().filter(|e| e["event"] == event).count()
    }

    pub fn snapshot(&self) -> Vec<Value> {
        self.events.lock().unwrap().clone()
    }
}
