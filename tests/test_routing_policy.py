import pytest

from agent.agent import Agent
from agent.events import AgentEventType
from agent.routing_policy import classify
from config.config import ApprovalPolicy, Config
from tests.fakes import ScriptedClient, call, text


@pytest.mark.parametrize("req,tier", [
    ("rename the helper getUser to fetchUser", "fast"),
    ("where is the discount code validated?", "fast"),
    ("fix the race condition in booking expiry", "strong"),
    ("investigate why payments fail intermittently", "strong"),
    ("refactor the cart module", "strong"),
    ("update apps/server/src/a.ts, apps/server/src/b.ts, lib/c.ts and lib/d.ts to use the new type", "strong"),
    ("add a loading spinner to the events page", "default"),
    ("x" * 800, "strong"),
])
def test_classifier(req, tier):
    assert classify(req)[0] == tier


async def run(cfg, script, msg):
    seen = []

    async with Agent(cfg) as a:
        client = ScriptedClient(script)
        orig = client.chat_completion

        async def spy(messages, tools=None, stream=True, role="default"):
            seen.append(role)
            async for e in orig(messages, tools, stream, role):
                yield e

        client.chat_completion = spy
        a.session.client = client
        evs = [e async for e in a.run(msg)]
    return seen, evs


ROUTES = {"fast": ["flash"], "strong": ["big"]}


async def test_each_request_uses_the_tier_its_difficulty_calls_for(tmp_path):
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, check_references=False, routing=ROUTES)
    roles, evs = await run(cfg, [text("ok")], "rename the helper getUser to fetchUser")
    assert roles == ["fast"]
    assert any("fast model" in e.data["message"] for e in evs if e.type == AgentEventType.NOTICE)
    roles, _ = await run(cfg, [text("ok")], "debug the intermittent failure")
    assert roles == ["strong"]
    roles, _ = await run(cfg, [text("ok")], "add a spinner to the page")
    assert roles == ["default"]


async def test_pinned_model_or_missing_chains_disable_routing(tmp_path):
    pinned = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, check_references=False, routing=ROUTES, model_pinned=True)
    assert (await run(pinned, [text("ok")], "rename x to y"))[0] == ["default"]
    no_chains = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, check_references=False)
    assert (await run(no_chains, [text("ok")], "rename x to y"))[0] == ["default"]


async def test_failed_verification_escalates_to_the_strong_model(tmp_path):
    import subprocess

    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO, check_references=False, routing=ROUTES,
                 verify_commands=["test -f ok.txt"])
    script = [call("write_file", {"path": "bad.txt", "content": "1"}), text("done"), call("write_file", {"path": "ok.txt", "content": "1"}, "c2"), text("fixed")]
    roles, _ = await run(cfg, script, "rename x to y")
    assert roles == ["fast", "fast", "strong", "strong"]  # starts cheap, escalates once a repair is needed
