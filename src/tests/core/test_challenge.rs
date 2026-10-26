//! Ported from tests/test_challenge.py: the challenger's objections are cited or dropped.
use crate::agent::challenge::{is_risky, keep_grounded, parse_objections, render};

const REPLY: &str = "OBJECTIONS:
1. [verified] drops the unique index | evidence: db/schema.sql:2
2. [verified] claims a rule in ghost/file.py | evidence: ghost/file.py:9
3. [verified] says it without any citation | evidence: none
4. [plausible] callers may break | evidence: none
5. [guess] an old client may send it | evidence: none
";

#[tokio::test]
async fn cited_or_dropped_and_uncited_claims_never_read_as_facts() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("db")).unwrap();
    std::fs::write(tmp.path().join("db/schema.sql"), "create table u();\ncreate unique index i on u(a);\n").unwrap();
    let (kept, dropped) = keep_grounded(parse_objections(REPLY), tmp.path()).await;
    let got: Vec<(String, String)> = kept.iter().map(|o| (o.confidence.clone(), o.text.chars().take(10).collect())).collect();
    assert_eq!(got, vec![("verified".to_string(), "drops the ".to_string()), ("guess".into(), "callers ma".into()), ("guess".into(), "an old cli".into())]);
    assert_eq!(dropped, 2); // the invented file and the uncited "verified"
    assert!(render(&kept, dropped).contains("2 more removed"));
}

#[test]
fn risky_requests_are_the_design_choices_not_small_fixes() {
    assert!(is_risky("should we replace the queue with cron instead of workers?"));
    assert!(is_risky("I think we should migrate to postgres"));
    assert!(!is_risky("fix the typo in the header"));
    assert!(parse_objections("OBJECTIONS: NONE").is_empty());
}
