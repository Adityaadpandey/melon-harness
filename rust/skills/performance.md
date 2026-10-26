---
name: performance
description: Measure first; fix the real bottleneck; prove the gain
triggers: slow, latency, performance, battery, cpu usage, cpu, energy, power usage, polling, wakeups, optimize, optimise, optimization, optimisation, speed up, n+1, memory leak, throughput, profile, profiling, benchmark, bottleneck, lag, laggy, re-render, bundle size
---
# Performance
Measure before changing anything and name the metric you expect to move (request time, query count, memory, render count, bundle KB). Without a number you are guessing; if you cannot measure, say "estimated" and why.
- Find where the time goes: a profiler, `EXPLAIN ANALYZE`, query logging, timing around suspects, the browser performance panel. Fix the biggest slice first; a 5% loop is not worth touching while one query takes 2s.
- Usual suspects: N+1 queries (one per row), unbounded reads, missing indexes or an index the query cannot use (a function on the column), serial awaits that could run together, repeated parsing or JSON round trips, work inside loops, oversized payloads, unnecessary re-renders, memory held by a cache with no bound or by listeners never removed.
- Cheapest first: do less work (select only needed columns, paginate, early return), then do it once (batch, memoise, cache with a clear invalidation rule), then do it elsewhere (index, queue, background job). A cache without an invalidation story is a future bug.
- Change one thing, re-measure on the same input, keep the change only if it moved the number. Verify behaviour did not change (the tests), because a fast wrong answer is a regression.
Example: a list page makes 1 + N queries for author names. Replace with one join or `IN (...)` fetch, then report "41 queries -> 2, 380ms -> 45ms on the same data". Say which numbers you measured and which you reasoned. Combined with another playbook: one list, each item tagged with its lens.
