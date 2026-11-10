# Desktop renderer

The React application in this directory uses the controlled `window.centaerisHost`
bridge supplied by Desktop. Main-process services and preload expose native
capabilities while Core and Agent own execution semantics.

Shared primitives and visualization rendering come from `@centaeris/ui` and
`@centaeris/visuals`. Product layout, transcript presentation and asynchronous
request lifetimes belong to this renderer.

Run the development server from the repository root.

```powershell
pnpm --filter @centaeris/electron-host run dev:renderer
```

The Desktop package also provides `build:renderer`, `typecheck`, `test:renderer`
and `gate:renderer`. Renderer output is written to `renderer-dist` beside the
Desktop manifest and bundled into the application's `resources/ui-dist`.

See [Architecture](../../../../docs/architecture/Architecture.md) for the host
boundary and [Frontend foundations](../../../../docs/development/FrontendFoundations.md)
for shared frontend ownership.
