//! Ported from tests/test_search_listing.py: generated folders stay out of file listings, a listing is reused
//! within one agent round, and the search index can be warmed in the background.
use super::git;
use crate::util::search;
use std::fs;
use std::time::Duration;

#[test]
fn generated_folders_are_not_listed_and_a_listing_is_reused_until_a_write() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    git(p, &["init", "-q"]);
    fs::write(p.join("a.ts"), "x").unwrap();
    fs::create_dir(p.join("graphify-out")).unwrap();
    fs::write(p.join("graphify-out/graph.json"), "{}").unwrap();
    search::forget_listing();
    let names = |p: &std::path::Path| -> Vec<String> { search::list_files(p, &[]).iter().map(|f| f.file_name().unwrap().to_string_lossy().to_string()).collect() };
    assert_eq!(names(p), vec!["a.ts"]);
    fs::write(p.join("b.ts"), "y").unwrap();
    // outside an agent round nothing is reused
    assert_eq!(search::list_files(p, &[]).len(), 2);
    search::hold_listing(true);
    assert_eq!(search::list_files(p, &[]).len(), 2);
    fs::write(p.join("c.ts"), "z").unwrap();
    // same round: the first listing is reused
    assert_eq!(search::list_files(p, &[]).len(), 2);
    // the registry does this after any write
    search::forget_listing();
    assert_eq!(search::list_files(p, &[]).len(), 3);
    search::hold_listing(false);
}

#[tokio::test]
async fn warm_builds_the_index_in_the_background() {
    // Python peeks at rank._cache; the Rust index is private, so check the observable result: after warming,
    // a search for the file's own words finds it.
    use crate::tools::builtin::search::SearchTool;
    use serde_json::json;
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    git(p, &["init", "-q"]);
    fs::write(p.join("pay.ts"), "export function chargeInvoice() {}").unwrap();
    crate::util::rank::warm(p.to_path_buf(), vec![]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let r = super::run_tool(&SearchTool::new(super::handle(p)), p, json!({"query": "charge invoice"})).await;
    assert!(r.output.contains("pay.ts"), "{}", r.output);
}
