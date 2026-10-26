//! Ported from tests/test_json_mode.py
use crate::config::ApprovalPolicy;
use crate::tests::fakes::*;
use crate::ui::app::{Cli, TEST_SCRIPT};
use crate::ui::sink;
use serde_json::{json, Value};

#[tokio::test]
async fn json_mode_prints_one_valid_event_per_line() {
    let _g = super::SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
    *TEST_SCRIPT.lock().unwrap() = Some(vec![call("read_file", json!({"path": "a.txt"})), text("done")]);
    let mut cfg = config(dir.path());
    cfg.approval = ApprovalPolicy::Auto;
    let mut cli = Cli::new(cfg);
    sink::install(); // catch what would go to stdout
    let ok = cli.run_json("go").await;
    let printed = sink::uninstall();
    assert!(ok);
    let events: Vec<Value> = printed.lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not json: {l:?} ({e})"))).collect();
    let kinds: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert!(kinds[0] == "agent_start" && *kinds.last().unwrap() == "agent_end", "{kinds:?}");
    assert!(kinds.contains(&"tool_call_start") && kinds.contains(&"tool_call_complete"));
}
