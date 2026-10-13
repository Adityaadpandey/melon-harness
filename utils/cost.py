"""Usage and cost summary from the trace. Cost is only computed for models the
user priced in config; unknown prices stay unknown rather than guessed."""
from __future__ import annotations


NOISE_FLOOR = 1024  # smaller shortfalls are cache-breakpoint granularity, not misses
CACHE_TTL_SEC = 300  # providers drop an idle prompt cache after ~5 minutes


def cache_waste(events: list[dict], pricing: dict) -> dict:
    """Prompt tokens that were in the previous request's prompt but were not
    read from cache this time, with the likely reason. Only counted for models
    that report caching at all (otherwise 'no cache' is not a miss)."""
    prev = None  # (prompt_tokens, model, end_time)
    reported: set[str] = set()
    misses, tokens, cost = [], 0, 0.0
    for e in events:
        if e.get("event") == "compaction":
            prev = None  # history was rewritten on purpose; a fresh prefix is expected
            continue
        if e.get("event") != "model_call" or not e.get("ok") or not e.get("tokens"):
            continue
        t = e["tokens"]
        prompt, cached, model = t.get("prompt_tokens", 0), t.get("cached_tokens", 0), e["model"]
        if cached:
            reported.add(model)
        if prev and model in reported:
            shortfall = prev[0] - cached
            if shortfall > NOISE_FLOOR:
                idle = (e["t"] - e.get("sec", 0)) - prev[2]
                why = "model changed" if prev[1] != model else ("idle > 5 min" if idle > CACHE_TTL_SEC else "prefix changed")
                misses.append({"tokens": shortfall, "why": why})
                tokens += shortfall
                price = pricing.get(model)
                if price and price.cached_input is not None:
                    cost += shortfall * (price.input - price.cached_input) / 1_000_000
        prev = (prompt, model, e["t"])
    return {"missed_tokens": tokens, "misses": len(misses), "reasons": sorted({m["why"] for m in misses}),
            "extra_cost_usd": round(cost, 4) if cost else None}


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
        "cache_waste": cache_waste(events, pricing),
    }
