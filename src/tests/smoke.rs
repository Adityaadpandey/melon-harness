use super::fakes::*;

#[tokio::test]
async fn scripted_agent_answers() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = scripted(config(dir.path()), vec![text("hello there")]).await;
    let ev = run(&mut a, "hi").await;
    assert_eq!(answer(&ev).as_deref(), Some("hello there"));
}

#[test]
fn mascot_is_seven_rows_of_sixteen_columns() {
    let rows = crate::ui::mascot::mascot();
    assert_eq!(rows.len(), 7);
    assert!(rows.iter().all(|r| crate::ui::style::strip_ansi(r).chars().count() == 16));
    assert!(crate::ui::mascot::svg(0, 0, 4).contains(r##"fill="#3f7d3a""##));
}
