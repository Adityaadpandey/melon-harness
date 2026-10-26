//! Full copies of tool results that left the model's context, so `recall_result` can hand back the exact text.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Default)]
struct Inner {
    mem: HashMap<String, String>,
    dir: Option<PathBuf>,
    n: usize,
}

#[derive(Default)]
pub struct ResultStore(Mutex<Inner>);

impl ResultStore {
    pub fn new(dir: Option<PathBuf>) -> ResultStore {
        ResultStore(Mutex::new(Inner { dir, ..Default::default() }))
    }

    /// Keep `text`, return its id (`r1`, `r2`...). Saving to disk is best effort; memory always works.
    pub fn put(&self, text: &str) -> String {
        let mut s = self.0.lock().unwrap();
        s.n += 1;
        let rid = format!("r{}", s.n);
        s.mem.insert(rid.clone(), text.to_string());
        if let Some(d) = &s.dir {
            if std::fs::create_dir_all(d).is_ok() {
                let _ = std::fs::write(d.join(format!("{rid}.txt")), text);
            }
        }
        rid
    }

    /// A resumed session keeps writing into its own folder; new ids continue after the highest one there.
    pub fn move_to(&self, dir: &Path) {
        let mut s = self.0.lock().unwrap();
        s.dir = Some(dir.to_path_buf());
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if let Some(num) = name.strip_prefix('r').and_then(|r| r.strip_suffix(".txt")).and_then(|n| n.parse::<usize>().ok()) {
                    s.n = s.n.max(num);
                }
            }
        }
    }

    pub fn get(&self, rid: &str) -> Option<String> {
        let s = self.0.lock().unwrap();
        if let Some(t) = s.mem.get(rid) {
            return Some(t.clone());
        }
        if rid.chars().all(|c| c.is_ascii_alphanumeric()) {
            if let Some(d) = &s.dir {
                return std::fs::read_to_string(d.join(format!("{rid}.txt"))).ok();
            }
        }
        None
    }
}
