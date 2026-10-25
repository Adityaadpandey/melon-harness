use super::fakes::*;

#[tokio::test]
async fn scripted_agent_answers() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = scripted(config(dir.path()), vec![text("hello there")]).await;
    let ev = run(&mut a, "hi").await;
    assert_eq!(answer(&ev).as_deref(), Some("hello there"));
}
