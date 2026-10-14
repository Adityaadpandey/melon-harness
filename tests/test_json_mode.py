import json

import main
from agent.agent import Agent
from config.config import ApprovalPolicy, Config
from tests.fakes import ScriptedClient, call, text


async def test_json_mode_prints_one_valid_event_per_line(tmp_path, capsys, monkeypatch):
    (tmp_path / "a.txt").write_text("hi")

    class Scripted(Agent):
        async def __aenter__(self):
            await super().__aenter__()
            self.session.client = ScriptedClient([call("read_file", {"path": "a.txt"}), text("done")])
            return self

    monkeypatch.setattr(main, "Agent", Scripted)
    cli = main.CLI(Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO))
    assert await cli.run_json("go") is True
    events = [json.loads(line) for line in capsys.readouterr().out.splitlines()]
    kinds = [e["type"] for e in events]
    assert kinds[0] == "agent_start" and kinds[-1] == "agent_end"
    assert "tool_call_start" in kinds and "tool_call_complete" in kinds
