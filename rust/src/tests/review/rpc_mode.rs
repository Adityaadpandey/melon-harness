//! Ported from tests/test_rpc_mode.py
use crate::config::ApprovalPolicy;
use crate::tests::fakes::*;
use crate::ui::app::{Cli, TEST_SCRIPT};
use crate::ui::sink;
use serde_json::{json, Value};

#[tokio::test]
async fn rpc_prompt_streams_events_and_every_command_is_answered() {
    let _g = super::SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    *TEST_SCRIPT.lock().unwrap() = Some(vec![text("done")]);
    let mut cfg = config(dir.path());
    cfg.approval = ApprovalPolicy::Auto;
    let mut cli = Cli::new(cfg);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tx.send(format!("{}\n", json!({"type": "prompt", "message": "go", "id": "1"}))).unwrap();
    tx.send(format!("{}\n", json!({"type": "steer", "message": "x"}))).unwrap(); // already ended or still running, either way answered
    tx.send("not json\n".into()).unwrap();
    tx.send(format!("{}\n", json!({"type": "nope"}))).unwrap();
    drop(tx); // stdin closed: the loop finishes the running request and ends
    sink::install();
    cli.run_rpc_lines(rx).await;
    let printed = sink::uninstall();
    let out: Vec<Value> = printed.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let kinds: Vec<&str> = out.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(kinds.iter().filter(|k| **k == "response").count(), 4, "{kinds:?}");
    assert!(kinds.contains(&"agent_start") && kinds.contains(&"agent_end"));
    assert_eq!(out.iter().find(|e| e["command"] == "prompt").unwrap()["success"], true);
    let responses: Vec<&Value> = out.iter().filter(|e| e["type"] == "response").collect();
    assert_eq!(responses.last().unwrap()["success"], false); // unknown command
}
