---
name: tunyt-debugging
description: Debugging runbook for bugs, 500s, failing specs, stuck BullMQ jobs, stale cache, slowness, and "green deploy but broken prod". Classify the failure lane first.
triggers: bug, error, 500, 502, 503, 401, 403, 404, fails, failing, failed, crash, broken, not working, doesn't work, stuck, never ran, never sent, stale, wrong amount, mismatch, regression, timeout, timed out, exception, duplicate
canonical: docs/skills/tunyt-debugging.md
---
# Debugging playbook (Tunyt)

Discipline, in order. These are not optional:
1. **Reproduce before fixing.** State the repro (command, request, spec). No fix for a failure you haven't seen.
2. **One hypothesis at a time.** State it, test it with one command, keep or drop it. Never shotgun several edits.
3. **Root cause, not symptom.** A swallowed catch, timeout/TTL bump, retry, `?? {}` fallback or cache flush is not a fix. The real error is in the logs (`wrapDatabaseOperation` logs the raw Prisma error; the HTTP body is deliberately generic).
4. **Two failed fixes = stop**, re-classify the lane, pick a different one.
5. Never wipe state first (worker restart, cache flush, DB reset): capture evidence first.
6. Silent failure (no JS error, single-digit-ms response) -> suspect nginx rate limiting / `proxy_next_upstream` retries BEFORE Express or React.
7. You never touch a DB and never read `.env*`. Need data? Write the query, say **where** (local/prod), **read or write** (writes wrapped in `BEGIN; ... ROLLBACK;`), and **what to paste back**. Use `<placeholders>`, never real emails/tokens.

## Step 1: classify the lane (then read that lane's section)
| Symptom | Lane | First action | `use_skill` section |
|---|---|---|---|
| HTTP 4xx/5xx from API | server-http | find the `requestId`/log line; error body is sanitized by design | "Server-HTTP lane" |
| Response OK but async effect missing (email, notification, AI draft, refund) | worker | is the worker process up? `pnpm dev` alone does NOT run jobs; check queue + kill-switch (10 respawns/min) | "Worker lane" |
| Everything slow at once, unrelated tables stall equally | database | instance saturation, not one slow query: ask user for `pg_stat_statements` | "Database lane" |
| Web page broken/hydration/build | web | `rm -rf apps/web/.next` after branch switch; `pnpm typecheck` checks app AND `src/sw` | "Web lane" |
| CRM redirected to /login | crm | cookie + proxy | "CRM lane" |
| Container down / 502 | infra | `docker service ls/ps` | "Infra lane" |
| Green deploy, prod broken | post-deploy | nginx change? migration not run? worker crash-looping while API healthy? | "Post-deploy lane" |
| Paid amount != booking total | payment | Payment row is never repriced by `repriceWithDiscount`; compare captured amount vs `booking.total` | "Payment lane" |
| Refund/revenue numbers disagree by one refund | analytics | refund aggregates must scope `ticket: { bookingId: { not: null } }`; grep `prisma.refund.aggregate` project-wide | "Payment lane" |
| Team booking fee/split wrong | team | | "Team booking lane" |
| Coupon/discount, venue PATCH, venue staff, analytics source, walk-in UPI, abandoned list | named lanes | | "Discount/coupon lane", "Venue/resource PATCH lane", "Venue staff/manager lane", "Analytics / traffic-source lane", "Walk-in UPI lane", "Audience" |

Pre-flight (30s, catches most "bugs"): prisma client stale -> `cd apps/server && npx prisma generate` (safe); worker not running; local docker services down (`docker-compose.local.yaml`, not stag); a server route renamed but client path not (grep `apps/web/src/lib/redux/api/`); e2e auth failures mean backend/`ALLOW_DEV_OTP_BYPASS=true` missing, not a test bug; a hook crashing = missing Provider in the tree.

## Anti-patterns that are NOT fixes
swallowing errors; bumping timeout/TTL; `setTimeout` to wait for a side effect (poll the state or use job completion); flushing cache instead of fixing missing invalidation at a write path; `retry` without idempotency (double capture/double send); tenant-scoping check AFTER the fetch (scope in the `WHERE`); `?? {}` to hide an undefined.

## Exit checklist
- root cause in one sentence backed by a log line/repro/plan; fix addresses it
- repro now passes; the matching Playwright spec ran if one exists
- server change: `npx tsc --noEmit` and `pnpm build` pass; endpoint touched -> Bruno `.yml` updated
- payment/auth path -> also followed `tunyt-security`
- new failure mode worth remembering -> say so, with file:line
- finish by asking whether to commit; never commit unasked
