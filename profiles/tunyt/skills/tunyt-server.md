---
name: tunyt-server
description: apps/server conventions: layer rule, house rules, adding an endpoint, auth middleware table, money, migrations, workers/AI.
triggers: endpoint, route, controller, service, repository, prisma, schema, migration, zod, server, api, webhook, booking, refund, payout, ticket, queue, worker, middleware, backend, express
canonical: .claude/skills/api-conventions/SKILL.md
---
# apps/server playbook

**Layer rule:** Route -> Controller -> Service -> Repository -> Database; never skip. Routes bind + middleware only. Controllers parse with Zod, call ONE service, reply via `SuccessHandler`, wrapped in `asyncHandler(fn, "Label")` (`requireUser(req)` first if authed). Services hold business logic and throw typed errors (`utils/errors.util.ts`), no `req/res`, no Prisma. Repositories: Prisma inside `wrapDatabaseOperation(...)`, no business rules. ~28 services still call `prisma.*` directly: move them when you're in the file, never add one.

House rules:
- Zod schema in `types/<domain>.types.ts`, type = `z.infer`; no new `validators/`.
- Errors are thrown, never returned; no `try/catch` that only re-throws.
- External calls (email/SMS/payments/OpenAI) go through a circuit breaker (`utils/circuit-breaker.util.ts`).
- **Money = integer paise** (`utils/money.util.ts`); no float rupee math. Fees only via `computeSeatPricing` (`fee-pricing.util.ts`), discounts via `computeDiscountSplit`; never hand-roll `platformfee + subtotal*perc` or read `discountedPrice`. Equal splits use largest-remainder rounding, never `Math.ceil(total/n)`.
- Counts are real: "N sold" = tickets with `status = SUCCESS`; "has user bought" checks `Ticket.userId`, not `Booking.userId` (team bookings differ).
- Tenant scoping goes IN the `WHERE`, not after the fetch.
- PATCH built from `Schema.partial()` must use `parsePartialUpdate` (`.partial()` keeps `.default()`s and resets unsent fields).
- Webhooks: `runWebhookOnce` (`utils/webhook-dedup.util.ts`), signatures via `crypto.timingSafeEqual`, secrets are required config keys. Never respond 200 then await the handler.
- `jwt.verify(token, secret, { clockTolerance: 10, algorithms: ["HS256"] })` always.
- Queues: bounded `removeOnComplete/removeOnFail`, idempotency key in payload for anything that sends or moves money. `pnpm dev` doesn't run workers.
- AI: pure code in `lib/ai/`, provider only via `lib/ai/shared/client.ts`, `sanitizeUserPrompt(...)` on every user prompt, assistant tools stay read-only.
- Files: kebab-case with suffix (`booking.service.ts`, `event.repository.ts`, `x.route.ts`, `x.types.ts`, `x.middleware.ts`, `x.util.ts`).

Adding an endpoint: route in `routes/v1/<domain>.route.ts` registered in `routes/v1/index.ts` (literal paths BEFORE param paths); controller -> service -> repository; Zod type; pick the right auth middleware (`use_skill tunyt-server section="Auth & Access Middleware"`: authMiddleware, requireEventAccess, checker/door-staff/college/venue/ops variants, `subscription-quota`); stricter rate limiter on OTP/login/expensive routes; **Bruno `.yml` in `BrunoCollection/<Domain>/` at repo root.** Keep audiences separate: a host token must not work on staff routes.

Database: you edit `schema.prisma` and write migration SQL; you may run `npx prisma generate`; you may NOT run `migrate dev|deploy|status|resolve|reset`, `db push`, `studio`. Applied migrations are immutable (fix forward). Schema + migration ship together. New filter -> index (`CONCURRENTLY` on busy tables). Tell the user plainly they must run `migrate status` then `migrate deploy` on prod; CI never does. `src/generated/prisma/` is generated, don't edit.
