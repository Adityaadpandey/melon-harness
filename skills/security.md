---
name: security
description: Checklist for auth, input handling, secrets, injection and permissions
triggers: auth, authentication, authorization, password, token, jwt, secret, injection, xss, csrf, sql injection, permission, encrypt, upload, cors, session, vulnerability, security
---
# Security checklist
Authorization on every path (who may do this; ownership checked in the query, not after); validate and bound all input at the boundary; parameterised queries only; no secrets in code, logs or errors; output encoded for its context; file uploads limited by type and size and never executed; rate limits on login, OTP and expensive endpoints; tokens verified with algorithm and expiry pinned; fail closed on errors. State the invariant you rely on, and list each item as checked, not applicable, or unverified.
