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
