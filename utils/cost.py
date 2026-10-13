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
                idle = (e.get("t", 0) - e.get("sec", 0)) - prev[2]
                why = "model changed" if prev[1] != model else ("idle > 5 min" if idle > CACHE_TTL_SEC else "prefix changed")
                misses.append({"tokens": shortfall, "why": why})
                tokens += shortfall
                price = pricing.get(model)
                if price and price.cached_input is not None:
                    cost += shortfall * (price.input - price.cached_input) / 1_000_000
        prev = (prompt, model, e.get("t", 0))
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
        "heaviest": heaviest_results(events),
    }


def heaviest_results(events: list[dict], top: int = 5) -> dict:
    """What made the prompts big. A tool result is re-sent with every later model
    call, so its real cost is its size x the calls that follow it (a compaction
    rewrites history, so it ends the count). Also lists repeated looks at one target."""
    rows, repeats, later = [], {}, 0
    for e in reversed(events):
        ev = e.get("event")
        if ev == "model_call" and e.get("ok"):
            later += 1
        elif ev == "compaction":
            later = 0
        elif ev == "tool_call" and e.get("chars"):
            tokens = e["chars"] // 4
            rows.append({"tool": e["tool"], "target": e.get("target", ""), "tokens": tokens, "carried": tokens * later})
            key = (e["tool"], e.get("target", ""))
            repeats[key] = repeats.get(key, 0) + 1
    rows.sort(key=lambda r: -r["carried"])
    return {
        "top": rows[:top],
        "tool_result_carried": sum(r["carried"] for r in rows),  # an estimate (chars/4): pruned results still count
        "repeated": [f"{t} {tgt} x{n}" for (t, tgt), n in repeats.items() if n > 1 and t in ("read_file", "grep", "search")][:5],
    }
