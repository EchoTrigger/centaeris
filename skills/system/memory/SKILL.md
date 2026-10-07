---
name: memory
description: Use when durable private context about this user and Agent may help, or when asked to remember, update, correct, or forget it.
allowed-tools: [read, edit, write]
---

# Memory

Use Agent Memory only when it is relevant to the current task. Do not read it routinely.

## Read

1. List `plastic-memories://self/` when you need to discover available memory.
2. Read `plastic-memories://self/MEMORY.md` for the concise index, then read only the relevant topic files.
3. Treat current user instructions and current task facts as more authoritative than saved memory. Surface conflicts instead of silently choosing stale memory.

## Write

Update Memory proactively when the current work establishes durable, user-specific preferences, decisions, relationships, or ongoing project context that will materially help a later Session.

1. Keep `plastic-memories://self/MEMORY.md` as a short index of Markdown links and one-line descriptions.
2. Store details in `plastic-memories://self/topics/<lower-kebab>.md`; topic slugs are 1–64 ASCII lowercase letters or digits separated by single hyphens.
3. Read an existing file before changing it. Update an existing topic before creating a duplicate.
4. Write the topic first and update the index second, so the index never points to content that was not written.
5. Record concise facts and their useful context. Do not save transient tool output, speculative inferences, conversation transcripts, or facts that are already obsolete.
6. When the user corrects or asks to forget a fact, remove it from the topic and index rather than preserving it as history.

Use only these canonical URIs:

- `plastic-memories://self/`
- `plastic-memories://self/MEMORY.md`
- `plastic-memories://self/topics/`
- `plastic-memories://self/topics/<lower-kebab>.md`

There is no manifest, shared Memory, alternate authority, query, fragment, or physical-path interface.
