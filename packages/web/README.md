# Workspace Web

`packages/web` is the hosted product's Web frontend. Like the Desktop renderer,
it connects only to the Django REST/SSE control plane
([`packages/api`](../api/README.md)); it does not connect directly to Runtime,
Storage, Docker, Redis, Agent Memory, or model-provider credentials.

See [Architecture](../../docs/architecture/Architecture.md),
[Workspace API](../../docs/workspace/reference/API.md), and
[Web chat presentation](../../docs/workspace/reference/WebChatPresentation.md)
for typography roles, motion, and disclosure rules this frontend implements.

## Development

```powershell
pnpm --filter web run dev
```

## Gate

```powershell
pnpm --filter web run build
```

Runs lint, typecheck, and the Vite build. Package-scoped test subsets
(`test:chat-ui`, `test:unit`, etc.) are listed in `package.json`.
