//! Ported from tests/test_tool_schemas.py: no schema sent to the model points at a definition that is not sent.
use crate::tools::registry::create_default_registry;

#[test]
fn no_tool_schema_points_at_a_definition_that_is_not_sent() {
    let d = tempfile::tempdir().unwrap();
    let reg = create_default_registry(super::handle(d.path()));
    for schema in reg.get_schemas() {
        let text = schema.to_string();
        assert!(!text.contains("$ref") && !text.contains("$defs"), "{}", schema["name"]);
    }
}
