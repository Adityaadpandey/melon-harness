---
name: api-design
description: Endpoints, contracts, compatibility, idempotency and error shapes
triggers: endpoint, api, rest, contract, breaking change, webhook, route, request body, response shape, status code
---
# API changes
Find existing clients before changing a contract; prefer additive changes. Validate input at the edge and return consistent error shapes with the right status codes. Mutations that can be retried (payments, webhooks, jobs) need idempotency keys. Authorize in the handler path, not only in the UI. Update the API collection and docs the project keeps (Bruno, OpenAPI, README) in the same change.
