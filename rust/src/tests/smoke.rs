use super::fakes::*;

#[tokio::test]
async fn scripted_agent_answers() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = scripted(config(dir.path()), vec![text("hello there")]).await;
    let ev = run(&mut a, "hi").await;
    assert_eq!(answer(&ev).as_deref(), Some("hello there"));
}

#[test]
fn mascot_is_six_rows_of_fourteen_columns() {
    let rows = crate::ui::mascot::melon();
    assert_eq!(rows.len(), 6);
    assert!(rows.iter().all(|r| crate::ui::style::strip_ansi(r).chars().count() == 14));
    assert!(crate::ui::mascot::svg(0, 0, 4).contains(r##"fill="#3f7d3a""##));
}
