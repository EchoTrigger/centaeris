# Centaeris UI

`packages/ui` (`centaeris-ui`) is the host-agnostic Desktop renderer. It is a
React application shared by Local Host products — currently
[`packages/desktop`](../desktop/README.md) — over a controlled host bridge; it
does not talk to the Local Runtime Host protocol, the filesystem, or native
APIs directly.

Session, prompt, tool, Plugin, Skill, continuation, and runtime-job semantics
belong to Core and the Runtime hosts that adapt it; `ui` only renders the
canonical events and state a host bridge exposes.

The `react-i18next` foundation and English resources live in `src/i18n.ts` and
`src/locales/`. See [Architecture](../../docs/architecture/Architecture.md)
for the host/renderer boundary.

## Development

```powershell
pnpm --filter centaeris-ui run dev
```

## Gate

```powershell
pnpm --filter centaeris-ui run gate
```

Runs the build, lint, and vitest suite.
