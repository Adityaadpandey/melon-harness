import pytest

from client.llm_client import LLMClient
from client.providers import split
from client.response import StreamEvent, StreamEventType
from config.config import Config, ProviderConfig


def test_prefix_picks_a_provider_and_gateway_ids_are_untouched(tmp_path):
    cfg = Config(cwd=tmp_path)
    p, wire = split(cfg, "ollama:qwen3:8b")
    assert (p.name, wire, p.base_url) == ("ollama", "qwen3:8b", "http://localhost:11434/v1")  # the tag after the second colon survives
    assert split(cfg, "deepseek/deepseek-v4-flash") is None  # a gateway id, as before
    assert split(cfg, "nosuch:model") is None


def test_config_adds_a_provider_and_overrides_a_builtin(tmp_path, monkeypatch):
    monkeypatch.delenv("WORK_KEY", raising=False)
    cfg = Config(cwd=tmp_path, providers={
        "work": ProviderConfig(base_url="https://llm.example.com/v1", api_key_env="WORK_KEY", protocol="anthropic"),
        "ollama": ProviderConfig(base_url="http://gpu-box:11434/v1"),
        "broken": ProviderConfig(api_key_env="X"),
    })
    p, _ = split(cfg, "work:big")
    assert (p.protocol, p.api_key) == ("anthropic", None)
    assert "WORK_KEY" in cfg.missing_key("work:big")
    monkeypatch.setenv("WORK_KEY", "k")
    assert cfg.missing_key("work:big") is None
    assert split(cfg, "ollama:x")[0].base_url == "http://gpu-box:11434/v1"
    assert cfg.missing_key("ollama:x") is None  # local servers need no key
    assert any("providers.broken" in e for e in cfg.validate())


@pytest.mark.asyncio
async def test_client_sends_the_bare_model_name_to_the_providers_own_endpoint(tmp_path, monkeypatch):
    monkeypatch.setenv("GROQ_API_KEY", "gk")
    client = LLMClient(Config(cwd=tmp_path))
    seen = {}

    class Fake:
        async def stream(self, model, messages, tools, temperature):
            seen["model"] = model
            yield StreamEvent(type=StreamEventType.MESSAGE_COMPLETE)

    adapter = client._direct_adapter(split(client.config, "groq:llama-3.3-70b")[0])
    assert adapter._client.base_url.host == "api.groq.com" and adapter._client.api_key == "gk"
    client._adapters["groq"] = Fake()
    client.config.model_name = "groq:llama-3.3-70b"
    [ev async for ev in client.chat_completion([{"role": "user", "content": "hi"}])]
    assert seen["model"] == "llama-3.3-70b" and client._provider("groq:x") == "groq"
