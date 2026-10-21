"""Talk to model APIs directly, without the gateway.

`-m groq:llama-3.3-70b` sends the request straight to Groq using GROQ_API_KEY.
A model id without a known `name:` prefix (like `deepseek/deepseek-v4-flash`)
still goes through the gateway (API_KEY / BASE_URL) exactly as before.

Almost every model API speaks one of two wire formats, OpenAI chat or Anthropic
messages, so a provider is only a URL, the env var holding its key, and which
format it speaks. A provider that isn't listed below takes three lines in config.toml:

    [providers.work]
    base_url = "https://llm.internal.example.com/v1"
    api_key_env = "WORK_LLM_KEY"   # leave it out for a local server that needs no key
    protocol = "openai"            # or "anthropic"

The same table also overrides a built-in, e.g. pointing `ollama` at another machine.
"""
from __future__ import annotations

import os
from dataclasses import dataclass, field

import httpx


@dataclass
class Provider:
    name: str
    base_url: str
    api_key_env: str | None  # None = a local server that ignores keys
    protocol: str = "openai"  # "openai" or "anthropic"
    headers: dict[str, str] = field(default_factory=dict)

    @property
    def api_key(self) -> str | None:
        if not self.api_key_env:
            return "local"  # the SDKs refuse an empty key even when the server never checks it
        return os.environ.get(self.api_key_env) or None


PRESETS = {p.name: p for p in [
    Provider("openai", "https://api.openai.com/v1", "OPENAI_API_KEY"),
    Provider("anthropic", "https://api.anthropic.com", "ANTHROPIC_API_KEY", "anthropic"),
    Provider("gemini", "https://generativelanguage.googleapis.com/v1beta/openai", "GEMINI_API_KEY"),
    Provider("openrouter", "https://openrouter.ai/api/v1", "OPENROUTER_API_KEY"),
    Provider("groq", "https://api.groq.com/openai/v1", "GROQ_API_KEY"),
    Provider("deepseek", "https://api.deepseek.com/v1", "DEEPSEEK_API_KEY"),
    Provider("mistral", "https://api.mistral.ai/v1", "MISTRAL_API_KEY"),
    Provider("xai", "https://api.x.ai/v1", "XAI_API_KEY"),
    Provider("together", "https://api.together.xyz/v1", "TOGETHER_API_KEY"),
    Provider("fireworks", "https://api.fireworks.ai/inference/v1", "FIREWORKS_API_KEY"),
    Provider("cerebras", "https://api.cerebras.ai/v1", "CEREBRAS_API_KEY"),
    Provider("moonshot", "https://api.moonshot.ai/v1", "MOONSHOT_API_KEY"),
    Provider("zai", "https://api.z.ai/api/paas/v4", "ZAI_API_KEY"),
    Provider("ollama", "http://localhost:11434/v1", None),
    Provider("lmstudio", "http://localhost:1234/v1", None),
]}


def all_providers(config) -> dict[str, Provider]:
    """Built-ins with the user's [providers.*] tables laid over them."""
    out = dict(PRESETS)
    for name, c in (config.providers or {}).items():
        base = PRESETS.get(name)
        url = c.base_url or (base.base_url if base else None)
        if not url:
            continue  # config.validate() reports it
        env = c.api_key_env if "api_key_env" in c.model_fields_set else (base.api_key_env if base else None)
        out[name] = Provider(name, url, env, c.protocol or (base.protocol if base else "openai"), dict(c.headers))
    return out


def split(config, model_id: str) -> tuple[Provider, str] | None:
    """`groq:llama-3.3-70b` -> (groq, "llama-3.3-70b"). Only the first colon counts,
    so `ollama:qwen3:8b` keeps its tag. None when the prefix isn't a provider."""
    name, sep, rest = model_id.partition(":")
    if not (sep and rest):
        return None
    p = all_providers(config).get(name)
    return (p, rest) if p else None


async def fetch_models(p: Provider) -> list[str]:
    """The provider's own model list, which also proves the key works. Raises on failure."""
    root = p.base_url.rstrip("/")
    if p.protocol == "anthropic":
        url = root + ("/models" if root.endswith("/v1") else "/v1/models")
        headers = {"x-api-key": p.api_key or "", "anthropic-version": "2023-06-01"}
    else:
        url, headers = root + "/models", {"Authorization": f"Bearer {p.api_key}"}
    async with httpx.AsyncClient(timeout=15) as c:
        r = await c.get(url, headers={**headers, **p.headers})
    r.raise_for_status()
    return sorted(m["id"] for m in r.json().get("data", []) if m.get("id"))
