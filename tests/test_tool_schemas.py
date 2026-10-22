import json

from config.config import Config
from tools.registry import ToolRegistry


def test_no_tool_schema_points_at_a_definition_that_is_not_sent():
    reg = ToolRegistry(Config(cwd="/tmp"))
    for schema in reg.get_schemas():
        text = json.dumps(schema)
        assert "$ref" not in text and "$defs" not in text, schema["name"]
