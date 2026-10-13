---
name: refactor
description: Behavior-preserving restructuring: find callers, keep contracts, verify before and after
triggers: refactor, rename, extract, restructure, clean up, cleanup, reorganize, simplify, split, move, deduplicate
---
# Refactor safely
Behavior must not change. Find every caller and importer first (grep, graph) and keep public names and signatures unless asked to change them. Run the relevant tests BEFORE to know the baseline, change in small steps, and run them after. Move code verbatim before editing it. Do not mix a refactor with a feature or bug fix. Delete code that became dead; do not leave shims "for later".
