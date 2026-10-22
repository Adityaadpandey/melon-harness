from agent import orient as o
from client.response import StreamEvent, StreamEventType, TextDelta


def test_the_reply_is_read_into_an_orientation_and_junk_is_ignored():
    got = o.parse('sure {"intent": "review", "tier": "default", "where": ["apps/server/src/otp"], "queries": ["otp expiry"], "note": "the uncommitted change"}')
    assert got.intent == "review" and got.where == ["apps/server/src/otp"] and "otp expiry" in got.hint()
    assert o.parse("no json here") is None
    assert o.parse('{"intent": "banana", "tier": "huge"}').intent == "other"


def test_repo_map_shows_folders_recent_and_uncommitted_work(tmp_path):
    import subprocess
    subprocess.run(["git", "init", "-q"], cwd=tmp_path)
    (tmp_path / "apps" / "web" / "src").mkdir(parents=True)
    (tmp_path / "apps" / "web" / "src" / "a.ts").write_text("x")
    (tmp_path / "README.md").write_text("hi")
    m = o.repo_map(tmp_path)
    assert "apps/web" in m and "README.md" in m and "UNCOMMITTED" in m and "a.ts" in m


async def test_a_failed_call_means_no_orientation():
    class Down:
        async def chat_completion(self, *a, **k):
            yield StreamEvent(type=StreamEventType.ERROR, error="boom")

    assert await o.orient(Down(), "fast", "review the change i have", "FOLDERS: x") is None


async def test_the_fast_model_answer_is_used():
    class Fast:
        async def chat_completion(self, *a, **k):
            yield StreamEvent(type=StreamEventType.MESSAGE_COMPLETE, text_delta=TextDelta('{"intent": "review", "tier": "fast"}'))

    got = await o.orient(Fast(), "fast", "review the change i have", "FOLDERS: x")
    assert got.intent == "review" and got.tier == "fast"


def test_what_the_change_should_land_in_is_read_from_the_reply():
    assert o.parse('{"intent": "change", "touches": "code"}').touches == "code"
    assert o.parse('{"intent": "change", "touches": "poetry"}').touches == "none"


async def test_a_bug_fix_that_only_edited_a_doc_is_sent_back(tmp_path):
    import subprocess

    from agent.agent import Agent
    from agent.events import AgentEventType
    from config.config import ApprovalPolicy, Config
    from tests.fakes import ScriptedClient, call, text

    subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
    cfg = Config(cwd=tmp_path, approval=ApprovalPolicy.AUTO, auto_verify=False, check_references=False)
    script = [call("write_file", {"path": "NOTES.md", "content": "n"}), text("noted"), call("write_file", {"path": "fix.ts", "content": "x"}, "c2"), text("fixed")]
    async with Agent(cfg) as a:
        a.session.client = ScriptedClient(script)

        async def fake_orient(message):
            return o.Orientation(intent="change", tier="default", touches="code")

        a._orient = fake_orient
        evs = [e async for e in a.run("the invite link does not open, fix it please")]
    assert any("Only documentation changed" in str(e.data) for e in evs if e.type == AgentEventType.NOTICE)
    assert (tmp_path / "fix.ts").exists()
