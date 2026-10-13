---
name: tunyt-performance
description: Performance and scalability rules for list endpoints, Prisma queries, caches, BullMQ workers and page data fetching, each with an in-repo exemplar to copy.
triggers: slow, latency, performance, optimize, optimise, speed, cache, caching, query, findMany, pagination, paginate, index, n+1, queue, worker, throughput, p95, load, scale, redis, bullmq
canonical: docs/skills/tunyt-performance.md
---
# Performance playbook (Tunyt)

Every pattern is already proven in this repo. **Copy the exemplar, don't invent.** Search by name; line numbers drift.

Hard rules for new code:
1. **Every `findMany` is bounded** (`take` 200-10000 by purpose, or paginate). Unbounded was a P0.
2. **Pagination:** `skip=(page-1)*pageSize` + `Promise.all([findMany, count])` + `totalPages`. Exemplar `repositories/notification.repository.ts:47-66`.
3. **Hot reads = cache-aside, and EVERY mutation site invalidates.** Exemplar `getTicketTypesByEventId` + `delTicketTypeCache` (`lib/cache/ticket.ts`). Helpers `cacheGet/cacheSet/cacheDel/cacheDelPattern/buildCacheKey` in `lib/cache/index.ts`; key shape `cache:{type}:{identifier}`. A new writer without invalidation is the classic silent bug.
4. **Batch, don't loop:** N sequential `findById` -> one `{ in: ids }` query + a `Map`. Exemplar `payment.service.ts` -> `ticketRepository.findTicketsByIds`.
5. **No unbounded `Promise.all` over DB calls.** Pool is `DB_POOL_MAX` sliced per API/worker/realm (`config/db.ts`); a 14-query fan-out already saturated it once.
6. **No external call (Razorpay/Twilio/SES/OpenAI) inside a Prisma `$transaction`** (holds a pool connection for provider latency).
7. **New query filters need an index** (check `schema.prisma` `@@index`; add composite in the same change; busy tables use `CREATE INDEX CONCURRENTLY`, see `docs/db-saturation-runbook.md`).
8. **Heavy cached fan-outs need single-flight + stale-while-revalidate** (Redis NX lock; exemplar `analytics.repository.ts`).
9. **Batch maintenance loops sleep between batches** (250ms, `activity.repository.ts`) or they cause WAL/checkpoint storms.
10. **Windowed analytics read rollups** (`EventAnalyticsDaily`), only "today" scans live.

Web: hot pages prefetch in the RSC with `Promise.all` over `unstable_cache` fetches and seed RTK Query (`(event-detail)/events/[eventId]/page.tsx`); `unstable_cache` keys MUST include the dynamic id (missing it was a P0 data leak); per-user JWT data stays client-side; `next/image` with `fill`+`sizes`, `priority` only on LCP; memoize selectors. CRM: keep TanStack defaults, never poll faster than 10s, paginate tables > ~50 rows.

Budgets to respect: nginx zones api 200r/m, auth 10r/m, heavy 30r/m; AI workers concurrency 2; analytics worker 3; notification limiter 50/s; ticket-type cache 10m, plans 30m.

**Measure, don't guess.** Name the exact route/metric expected to move (`GET /api/v1/metrics`: `http_request_duration_seconds`, `bullmq_queue_waiting`, `circuit_breaker_state`). You cannot run SQL: hand the user `EXPLAIN (ANALYZE, BUFFERS) <SELECT>` (never with a write) and `pg_stat_statements`. Prisma does not log queries. Label any unmeasured gain "estimated" and say why.

Backlog lives in `PerformanceAudit.md` (R1-R15) and `AUDIT_FINDINGS.md`: extend those, don't rediscover; mark closed items there.
