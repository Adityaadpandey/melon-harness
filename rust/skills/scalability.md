---
name: scalability
description: What breaks at 10x load or data: state, hot paths, queues, limits and failure modes
triggers: scale, scalability, scalable, scaling, scales, high traffic, horizontal, million users, thousand users, 10x, capacity, sharding, shard, rate limit, backpressure
---
# Scalability
Ask what grows and how fast: users, rows, requests per second, payload size, fan-out per event. Then follow the largest one through the code and find where it stops being linear.
- Per-request cost: work that grows with table size (`SELECT *` without a limit, offset pagination on a deep page, a scan with no index), N+1 across a list, a loop that calls the network.
- State on the box: in-memory caches, sessions, files on local disk, locks held in the process. Two instances then disagree, and a restart loses it. Move it to the shared store or make it safe to lose.
- Fan-out and spikes: one event that triggers thousands of writes or emails, cron jobs that all fire on the hour, thundering herd after a cache expiry. Use a queue with a bounded worker count, jitter, and a limit on each outbound dependency.
- Limits you will hit first: DB connection pool, third-party rate limits, request timeouts, memory per worker. Name the one that breaks first and the number.
- Failure at load: when a dependency is slow, do requests pile up (add timeouts, a circuit breaker, load shedding) or fail fast? Retries without backoff multiply the outage.
- Writes: hot rows (a global counter every request updates), unique-key races, long transactions.

Give a rough number where you can (rows x bytes, requests x queries) rather than saying "might not scale". Rank fixes by effort versus headroom gained, and say what you would not do yet (sharding for 50k rows is premature). Do not rewrite for scale nobody asked for: an assessment names the first bottleneck and the cheapest fix. Combined with another playbook: one list, each item tagged with its lens.
