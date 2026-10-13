---
name: tunyt-security
description: Security checklist for auth, payments, webhooks, OTP, tenant isolation and any sensitive-data path.
triggers: auth, authentication, authorization, jwt, token, payment, razorpay, webhook, otp, tenant, permission, role, rbac, refund, payout, secret, csrf, cookie, security, idempotent, idempotency
canonical: .claude/skills/security-testing-compliance/SKILL.md
---
# Security playbook (payment / auth / sensitive data)

State the invariant you rely on before editing: **who may do this, and what must never apply twice.**

Check, in the diff and in the callers:
1. **Authorization:** right middleware for the caller (see `tunyt-server`); ownership/tenant enforced in the `WHERE`; no host token valid on staff routes; no check that runs only after the fetch.
2. **Money:** integer paise only; amount captured == `booking.total` before finalizing (verify path AND webhook path); never trust a client-sent amount.
3. **Idempotency:** webhooks through `runWebhookOnce`; jobs that send/charge carry an idempotency key; a retry must not double-capture/double-send; `proxy_next_upstream` retries are only safe on idempotent endpoints.
4. **Webhooks:** signature with `crypto.timingSafeEqual`; secrets are required config keys that fail startup in production, never `|| "default"`.
5. **Tokens:** `jwt.verify` with `{ clockTolerance: 10, algorithms: ["HS256"] }`; httpOnly cookies; new mutating web routes must work with the CSRF gate in `src/proxy.ts` (or be added to `CSRF_EXEMPT_PATHS` with a reason).
6. **Rate limits:** stricter limiter on OTP/login/expensive endpoints; check-in paths are deliberately exempt (non-idempotent, many guests behind one NAT).
7. **Data exposure:** no secrets/PII in logs or responses; private fields (e.g. private locations) redacted for non-owners; user prompts through `sanitizeUserPrompt`.
8. **Fail closed:** Redis/blacklist errors must deny (see AUDIT_FINDINGS #17), not allow.

Never read `.env*`, never run DB commands, never print secrets. In the report list each item above as checked / not applicable / could not verify.
