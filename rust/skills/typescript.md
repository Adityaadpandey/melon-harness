---
name: typescript
description: Writing TypeScript/React that type-checks cleanly and matches the project's idioms
triggers: typescript, ts, tsx, type error, tsc, interface, generic, zod, react, hook, nextjs, next, expo, react native, tanstack, redux, eslint, props
always: true
---
# TypeScript / React
Read `tsconfig.json` and a neighbouring file first; match its strictness, import style (path aliases, `import type`) and naming. Never add `any`, `as unknown as X`, `@ts-ignore` or non-null `!` to silence an error: narrow the type, fix the source type, or use `unknown` plus a guard. A `@ts-expect-error` needs a one-line reason.

Types: model states as discriminated unions (`{status:'ok',data}|{status:'error',error}`) instead of optional-field bags; derive types from values (`typeof`, `as const`, `z.infer`) instead of repeating them; prefer `interface` for object shapes you extend, `type` for unions. Validate data from the network, storage or `JSON.parse` at the boundary (zod if the repo has it) and trust the types after that. Exhaustive `switch` ends with a `never` check.

React: keep components pure and small; derive values during render instead of mirroring props into state; effects are for syncing with the outside world, not for computing data. List keys are stable ids, never the index. Server data goes through the project's data layer (RTK Query / TanStack Query), not hand-rolled `useEffect` + `fetch`. Type event handlers and props explicitly; no inline objects/functions in deps that make effects loop. In Next App Router, mark `'use client'` only on the leaf that needs it and keep secrets out of client bundles.

Before you say done: lint only the changed files (`eslint --quiet <files>`) and run their tests, once; report the real output. A change that compiles only because of a cast is not done.
