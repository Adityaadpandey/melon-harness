"""Probe which models the current API key can actually call.

Sends a tiny request to each model on its supported endpoint and prints
status + latency. Run: python scripts/probe_models.py [--json out.json]
"""
import asyncio, json, os, sys, time
import httpx
from dotenv import load_dotenv

load_dotenv()
BASE = os.environ.get("BASE_URL", "https://api.commandcode.ai/provider/v1")
KEY = os.environ["API_KEY"]
H = {"Authorization": f"Bearer {KEY}", "x-api-key": KEY, "anthropic-version": "2023-06-01"}


async def probe(c: httpx.AsyncClient, sem, m: dict) -> dict:
    eps = m["supported_endpoints"]
    if "/messages" in eps:
        url, body = f"{BASE}/messages", {"model": m["id"], "max_tokens": 16, "messages": [{"role": "user", "content": "Reply with: ok"}]}
    else:
        url, body = f"{BASE}/chat/completions", {"model": m["id"], "max_tokens": 64, "messages": [{"role": "user", "content": "Reply with: ok"}]}
    async with sem:
        t = time.perf_counter()
        try:
            r = await c.post(url, json=body, headers=H, timeout=60)
            dt = time.perf_counter() - t
            ok = r.status_code == 200
            err = "" if ok else r.text[:120].replace("\n", " ")
            return {"id": m["id"], "endpoint": url.rsplit("/", 1)[1], "ok": ok, "status": r.status_code, "sec": round(dt, 2), "err": err}
        except Exception as e:
            return {"id": m["id"], "ok": False, "status": 0, "sec": 0, "err": repr(e)[:120]}


async def main():
    async with httpx.AsyncClient() as c:
        models = (await c.get(f"{BASE}/models", headers=H)).json()["data"]
        sem = asyncio.Semaphore(6)
        res = await asyncio.gather(*(probe(c, sem, m) for m in models))
    for r in res:
        print(f"{'OK ' if r['ok'] else 'NO '} {r['status']} {r['sec']:>5}s {r['id']:<45} {r['err']}")
    print(f"\n{sum(r['ok'] for r in res)}/{len(res)} callable")
    if "--json" in sys.argv:
        json.dump(res, open(sys.argv[sys.argv.index("--json") + 1], "w"), indent=1)


asyncio.run(main())
