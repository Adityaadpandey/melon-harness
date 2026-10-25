//! Ported from tests/test_pi_reads.py
use crate::config::ConfigHandle;
use crate::tests::fakes::config;
use crate::tools::builtin::read_file::ReadFileTool;
use crate::tools::{Tool, ToolInvocation};
use serde_json::{json, Value};

#[tokio::test]
async fn pi_rereads_cost_one_line_or_a_diff() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("a.py");
    let body = (0..200).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
    std::fs::write(&f, &body).unwrap();
    let tool = ReadFileTool::new(ConfigHandle::new(config(dir.path())));
    let read = |epoch: i64, extra: Value| {
        let mut params = json!({"path": "a.py"});
        params.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
        let inv = ToolInvocation { params, cwd: dir.path().to_path_buf(), epoch: epoch as _, call_id: "c".into(), progress: None };
        let tool = &tool;
        async move { tool.execute(&inv).await.output }
    };
    assert!(read(0, json!({})).await.contains("line 199"));
    let again = read(0, json!({})).await;
    assert!(again.contains("unchanged") && !again.contains("line 100"));
    std::fs::write(&f, body.replace("line 5\n", "line five\n")).unwrap();
    let diff = read(0, json!({})).await;
    assert!(diff.contains("+line five") && !diff.contains("line 100"));
    assert!(read(1, json!({})).await.contains("line 100")); // compaction bumped the epoch: the model lost the text
    let part = read(1, json!({"offset": 90, "limit": 20})).await;
    assert!(part.contains("line 100") && !part.contains("unchanged"));
    let part2 = read(1, json!({"offset": 90, "limit": 20})).await;
    assert!(part2.contains("line 100") && !part2.contains("unchanged"));
}
