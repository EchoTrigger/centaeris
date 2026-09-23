---
name: runtime-recovery
description: Diagnose a specific degraded local Centaeris Runtime, Session, Plugin, Skill, or job and use only an available narrow repair.
---

# Runtime Recovery

Investigate the affected item without disturbing healthy Sessions or extensions.

## Workflow

1. Use the diagnostic code, identity, path, and preserved artifact actually supplied by the Host or user. Ask for a missing detail only when it is needed for the next check. Do not invent a diagnostic or search unrelated user data.
2. Confirm that the failure still exists with one relevant read-only check. Do not repeat a failed mutation or loop retries.
3. If the affected manifest or `SKILL.md` belongs to the user and is inside the current writable workspace, inspect it before a narrow edit. Preserve its previous contents through the workspace's normal version history or an authorized backup.
4. For an invalid Plugin or Skill source, identify the exact source and explain the available management action. Desktop has a Skills panel for Skill sources and Plugin controls; the native TUI has `/plugins` for Plugin enablement. These Host controls are not automatically model-visible tools. Only perform a configuration change when the active Host actually exposes an authorized action to this Agent.
5. Re-run the smallest available catalog, Session, or Runtime check that demonstrates the affected item is healthy. Report the diagnostic, any change made, and whether a restart is required. If no supported repair action is available, report the missing capability and leave the item unchanged.

## Boundaries

- There is no general Agent-facing command to force a Runtime job into the failed state. Escalate a permanently invalid job with its identity and diagnostic; do not change its storage directly.
- Recreate a missing directory only when the Host identifies that exact directory as safe to recreate and the active tools permit it.
- A user's request to repair one item does not grant access beyond the active Host's permissions.

## Never do automatically

- Do not delete or rewrite a Session log, database, user configuration, credential, workspace, or extension catalog.
- Do not edit SQLite tables directly, clear a Docker volume, or replace the whole Centaeris data directory.
- Do not relax public schema validation, add compatibility aliases, discard unknown fields, or fabricate a migration version.
- Do not print credential values or copy private data into a workspace, prompt, or public report.
- Do not repeat a mutation whose previous outcome is unknown. Inspect state first.

Released schema changes require the shipped forward migration or reconstruction from authoritative records. Preserve a damaged Session until a supported Host recovery path can validate the repair.
