//! Ported from tests/test_review_plausible.py
use crate::agent::review_pipeline as rp;

#[test]
fn plausible_survives_the_verifier_and_a_low_confidence_one_is_not_dropped() {
    let reply = "1 | PLAUSIBLE | a.sh:3 prune can fail under set -e | deploys | ONGOING | NEW | NONE | CONF 30";
    assert_eq!(rp::parse_verdicts(reply, 1)[0].0, "PLAUSIBLE");
    let mut f = vec![rp::Finding { severity: "MEDIUM".into(), where_: "a.sh:3".into(), what: "prune fails the step".into(), fix: "guard it".into(), ..Default::default() }];
    rp::apply_verdicts(&mut f, 0..1, reply, 60);
    assert_eq!(f[0].verdict, "PLAUSIBLE");
    assert!(rp::compose(&f, 1).contains("unverified"));
}
