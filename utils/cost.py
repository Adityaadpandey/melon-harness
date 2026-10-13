"""Usage and cost summary from the trace. Cost is only computed for models the
user priced in config; unknown prices stay unknown rather than guessed."""
from __future__ import annotations


def summarize_usage(events: list[dict], pricing: dict) -> dict:
    prompt = completion = cached = calls = 0
    cost: float | None = 0.0
    for e in events:
        if e.get("event") != "model_call" or not e.get("ok") or not e.get("tokens"):
            continue
        t = e["tokens"]
        calls += 1
        p, c, k = t.get("prompt_tokens", 0), t.get("completion_tokens", 0), t.get("cached_tokens", 0)
        prompt, completion, cached = prompt + p, completion + c, cached + k
        price = pricing.get(e["model"])
        if price is None or cost is None:
            cost = None
        else:
            cached_rate = price.cached_input if price.cached_input is not None else price.input
            cost += ((p - k) * price.input + k * cached_rate + c * price.output) / 1_000_000
    return {
        "calls": calls,
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "cached_tokens": cached,
        "cache_hit_pct": round(100 * cached / prompt, 1) if prompt else 0.0,
        "est_cost_usd": round(cost, 4) if cost is not None and calls else None,
    }
