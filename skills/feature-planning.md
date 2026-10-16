---
name: feature-planning
description: Designing or planning a new feature or change: options, trade-offs, a small first slice
triggers: new feature, add a feature, add feature, plan, planning, design, architecture, proposal, roadmap, how should we, how should i, approach for, trade-off, tradeoff, what's the best way, whats the best way
---
# Features and planning
Answer in the chat. Never write a plan, spec or design file unless the user asks for one, and ask first.
1. Ground it: read the code the feature touches and how the repo already does the nearest thing (routes, models, components, tests). A plan that ignores existing patterns is wrong before it starts.
2. Restate the goal in one line, and list assumptions you are making. Ask only about decisions that change the design and cannot be read from the code (who can use it, what happens to old data); otherwise pick a default and say it.
3. Give two options when there is a real choice, with the cost of each, and recommend one. Example: "notify by email now (an existing mailer, 1 file) or add a notification table (a migration, needed once you want in-app too). I'd start with the table only if in-app is on the roadmap."
4. Break the work into slices that each leave the app working: data and migration, backend, UI, tests. Name the files for each slice, and the first slice that can be verified alone.
5. Name what could go wrong: migrations on big tables, permission checks, existing callers, rollout and rollback (a flag, or a change that is safe to revert).
6. Say how you will know it works: the test, command or screen you will check.

Keep a plan to what the user can act on: no architecture essay for a small feature, and stop at the plan if they asked for one. When asked to build, build the first slice, verify it, then continue. Combined with another playbook: one list, each item tagged with its lens.
