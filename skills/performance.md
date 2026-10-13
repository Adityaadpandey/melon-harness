---
name: performance
description: Measure first; fix the real bottleneck; prove the gain
triggers: slow, latency, performance, optimize, optimise, speed up, n+1, memory leak, throughput, profile, benchmark, bottleneck
---
# Performance
Measure before changing anything (timing, query count, profile) and name the metric you expect to move. Look for: N+1 queries, unbounded reads, work inside loops, repeated parsing, missing indexes, serial awaits that could be parallel, oversized payloads. Change one thing, re-measure on the same input. Report before and after numbers; if you could not measure, call the gain "estimated" and explain why.
