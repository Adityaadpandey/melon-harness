---
name: tunyt-web-ui
description: apps/web and apps/crm rules: the UI Guard (ask before visible changes), API proxy, CSRF gate, design skill pointer.
triggers: ui, component, page, css, tailwind, design, frontend, layout, button, modal, form, screen, web, crm, nextjs, react, redux, style
canonical: docs/skills/tunyt-web-design.md
---
# Web / CRM playbook

**UI Guard: ask the user BEFORE** adding a visible component/page/section/route, restyling (color, spacing, layout, type), changing user-facing copy/labels/order, or swapping icons/imagery. **Allowed without asking:** an obvious visual bug (broken layout, overflow), non-visual logic inside an existing component (handlers, state, API calls), import/type fixes after a rename. When unsure, ask. Ask in plain text BEFORE the first write_file/edit that adds or restyles anything visible (e.g. a spinner, overlay or progress bar): a rejected change is not an answer, and each guess you write costs a round. Prefer the root cause (e.g. `prefetch={false}` on a link) over a new component. In a non-interactive run, do the non-visual part only and list the visual changes you did NOT make.

Web (`apps/web`, Next 16 App Router, React 19, Tailwind v4 tokens under `@theme` in `globals.css`, no tailwind.config): route handlers in `src/app/api/**` stay thin proxies via `src/app/api/_lib/upstream.ts`; `src/proxy.ts` is the CSRF gate (new mutating route that can't send the header -> `CSRF_EXEMPT_PATHS` with a comment); RTK Query in `src/lib/redux/api/*Api.ts`, 401 -> single mutexed refresh via `/api/auth/me`. CRM: TanStack Query defaults are deliberate; Radix/shadcn components. Mobile (`apps/mobile`, Expo) has its own AGENTS.md: read it first.

Design work (only when explicitly asked): `use_skill tunyt-web-ui section=...` on `docs/skills/tunyt-web-design.md` (use section='list'), plus `tunyt-web-design-icon-paths.md` for icons (PikaIcons in web, Phosphor in CRM).
