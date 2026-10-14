import asyncio
import json

import main
from agent.agent import Agent
from config.config import ApprovalPolicy, Config
from tests.fakes import ScriptedClient, text


async def test_rpc_prompt_streams_events_and_every_command_is_answered(tmp_path, capsys, monkeypatch):
    class Scripted(Agent):
        async def __aenter__(self):
            await super().__aenter__()
            self.session.client = ScriptedClient([text("done")])
            return self

    monkeypatch.setattr(main, "Agent", Scripted)

    async def lines():
        yield json.dumps({"type": "prompt", "message": "go", "id": "1"}) + "\n"
        yield json.dumps({"type": "steer", "message": "x"}) + "\n"  # already ended or still running, either way answered
        yield "not json\n"
        yield json.dumps({"type": "nope"}) + "\n"

    cli = main.CLI(Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO))
    await cli.run_rpc(lines)
    out = [json.loads(line) for line in capsys.readouterr().out.splitlines()]
    kinds = [e["type"] for e in out]
    assert kinds.count("response") == 4
    assert "agent_start" in kinds and "agent_end" in kinds
    assert next(e for e in out if e.get("command") == "prompt")["success"] is True
    assert [e for e in out if e["type"] == "response"][-1]["success"] is False  # unknown command
