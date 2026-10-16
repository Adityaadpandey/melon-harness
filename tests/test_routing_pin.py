from client.routing import ModelRegistry, Router
from config.config import Config


def router(tmp_path, pinned):
    cfg = Config(cwd=tmp_path, routing={"strong": ["claude-sonnet-5-5"], "debug": ["claude-sonnet-5-5"], "fast": ["flash"]})
    cfg.model_name = "codex/gpt-5.6-luna"
    cfg.model_pinned = pinned
    return Router(cfg, ModelRegistry(cfg, tmp_path))


def test_escalation_and_the_debugger_stay_on_the_model_the_user_picked(tmp_path):
    r = router(tmp_path, pinned=True)
    assert r.chain("strong") == r.chain("debug") == ["codex/gpt-5.6-luna"]
    assert r.chain("fast")[0] == "flash"  # cheap routing for small roles is a cost choice, not a swap of the main model


def test_without_a_pinned_model_the_configured_strong_chain_still_applies(tmp_path):
    assert router(tmp_path, pinned=False).chain("strong")[0] == "claude-sonnet-5-5"
