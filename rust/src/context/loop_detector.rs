use serde_json::{Map, Value};
use std::collections::VecDeque;

pub struct LoopDetector {
    max_exact_repeats: usize,
    max_cycle_length: usize,
    history: VecDeque<String>,
}

impl Default for LoopDetector {
    fn default() -> Self {
        LoopDetector { max_exact_repeats: 3, max_cycle_length: 3, history: VecDeque::new() }
    }
}

impl LoopDetector {
    pub fn record_tool_call(&mut self, name: &str, args: &Map<String, Value>) {
        let mut parts = vec!["tool_call".to_string(), name.to_string()];
        let mut keys: Vec<&String> = args.keys().collect();
        keys.sort();
        for k in keys {
            let v = match &args[k] {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            parts.push(format!("{k}={v}"));
        }
        self.push(parts.join("|"));
    }

    pub fn record_response(&mut self, text: &str) {
        self.push(format!("response|{text}"));
    }

    fn push(&mut self, sig: String) {
        if self.history.len() == 20 {
            self.history.pop_front();
        }
        self.history.push_back(sig);
    }

    pub fn check_for_loop(&self) -> Option<String> {
        if self.history.len() < 2 {
            return None;
        }
        let h: Vec<&String> = self.history.iter().collect();
        if h.len() >= self.max_exact_repeats {
            let recent = &h[h.len() - self.max_exact_repeats..];
            if recent.iter().all(|x| *x == recent[0]) {
                return Some(format!("Same action repeated {} times", self.max_exact_repeats));
            }
        }
        if h.len() >= self.max_cycle_length * 2 {
            for cycle in 2..=(self.max_cycle_length.min(h.len() / 2)) {
                let recent = &h[h.len() - cycle * 2..];
                if recent[..cycle] == recent[cycle..] {
                    return Some(format!("Detected repeating cycle of length {cycle}"));
                }
            }
        }
        None
    }

    pub fn clear(&mut self) {
        self.history.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeats_and_cycles() {
        let mut d = LoopDetector::default();
        let a = Map::new();
        for _ in 0..3 {
            d.record_tool_call("read_file", &a);
        }
        assert!(d.check_for_loop().unwrap().contains("repeated"));
        let mut d = LoopDetector::default();
        for _ in 0..2 {
            d.record_tool_call("a", &a);
            d.record_tool_call("b", &a);
            d.record_tool_call("c", &a);
        }
        assert!(d.check_for_loop().unwrap().contains("cycle of length 3"));
    }
}
