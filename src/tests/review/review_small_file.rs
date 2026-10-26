//! Ported from tests/test_review_small_file.py
use crate::agent::review_pipeline as rp;
use crate::config::Config;

fn names(cfg: &Config, files: &[&str]) -> Vec<String> {
    let files: Vec<String> = files.iter().map(|s| s.to_string()).collect();
    rp::pick_lenses(cfg, &"+x\n".repeat(20), &files, "").into_iter().map(|(n, _)| n).collect()
}

#[test]
fn one_small_file_gets_one_reviewer() {
    let cfg = Config::default();
    assert_eq!(names(&cfg, &["a.yml"]), vec!["review"]);
    assert_eq!(names(&cfg, &["a.ts", "b.ts"]), vec!["review", "contracts"]);
}
