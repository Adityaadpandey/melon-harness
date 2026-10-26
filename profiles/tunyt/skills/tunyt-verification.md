---
name: tunyt-verification
description: What "done" means in Tunyt: per-app check commands, side-effect checklist, testing conventions.
triggers: verify, verification, done, finished, complete, typecheck, tsc, lint, build, playwright, e2e, test
canonical: docs/skills/tunyt-verification.md
---
# Verification playbook

**Evidence before claims.** Run the command, read the output, quote the relevant lines. Never say "passes/fixed/done" without it; list anything skipped.

Per app (run from the app dir):
- `apps/server`: `npx tsc --noEmit` (zero errors), `pnpm build`, `pnpm test` (Playwright logic specs, no browser). Schema changed -> `npx prisma generate`, migration file must exist, and hand the user `npx prisma migrate dev --name <name>` (say you did not run it).
- `apps/web`: `pnpm lint`, `pnpm typecheck` (app AND `src/sw`), `pnpm build`, `pnpm test:unit`. A booked flow changed -> matching e2e spec, which needs the API + DB up with `ALLOW_DEV_OTP_BYPASS=true`, so ASK the user first.
- `apps/crm`: `pnpm typecheck`, `pnpm build`. No tests; the linter is disabled by config, so lint is not evidence.

Side effects: endpoint added/changed/removed -> Bruno `.yml`; code changed -> `graphify update .`; payment/auth/sensitive -> `tunyt-security`; UI-visible change in web/crm -> UI Guard approval must already exist; need DB data or an env value -> give the user a query / ask, never fetch it.

Testing: **Playwright is the only runner** (never vitest/jest; `packages/tunyt-shared` is the one vitest exception). Don't write new tests during a fix round unless asked; run existing ones.
