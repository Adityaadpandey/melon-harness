//! Ported from tests/test_templates.py: prompt template arguments fill in with defaults and quoting.
use crate::util::templates::{expand, split_args};

#[test]
fn arguments_fill_in_with_defaults_and_quoting() {
    let body = "Review. Focus on ${1:-correctness}. Rest: ${@:2}. All: $@.";
    assert_eq!(expand(body, &split_args(r#""API compat" a b"#)), "Review. Focus on API compat. Rest: a b. All: API compat a b.");
    assert_eq!(expand(body, &[]), "Review. Focus on correctness. Rest: . All: .");
    assert_eq!(expand("x $1 y $ARGUMENTS", &["a".to_string(), "b".to_string()]), "x a y a b");
}
