//! How a tool asks the person at the keyboard something. The interactive app registers an `Asker`; without one (a
//! one-shot run) `ask_user` tells the model nobody can answer.
use futures::future::BoxFuture;
use std::sync::{Arc, Mutex, OnceLock};

pub trait Asker: Send + Sync {
    /// Show a headline (first line) with context under it, and the options; return the chosen index.
    fn ask_choice(&self, question: String, options: Vec<String>) -> BoxFuture<'static, usize>;
    fn read_line(&self, prompt: String) -> BoxFuture<'static, String>;
    fn say(&self, line: String);
}

fn slot() -> &'static Mutex<Option<Arc<dyn Asker>>> {
    static A: OnceLock<Mutex<Option<Arc<dyn Asker>>>> = OnceLock::new();
    A.get_or_init(|| Mutex::new(None))
}

pub fn set_asker(a: Option<Arc<dyn Asker>>) {
    *slot().lock().unwrap() = a;
}

pub fn asker() -> Option<Arc<dyn Asker>> {
    slot().lock().unwrap().clone()
}
