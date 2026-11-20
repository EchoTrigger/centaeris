# Frontend lint policy

The frontend lint gate distinguishes deterministic defects from audit signals.
The gate must not turn an unverified dependency-array rewrite into a production
change.

## Blocking rules

The following diagnostics are errors because fixing them does not require an
interpretation of runtime behavior:

- unreachable code;
- Hooks called outside the top level of a component or custom Hook;
- debugger statements;
- duplicate object keys and duplicate parameters.

## Audit rules

Unused imports and variables and exhaustive Hook dependencies are warnings.
They remain visible in normal lint output but do not block the local gate.

Do not apply unsafe lint fixes to dependency arrays. Before changing production
behavior for a Hook warning, add the smallest test that fails for the suspected
stale closure, missed refresh, duplicate subscription, reload loop, or rendering
regression. Intentional trigger dependencies stay in place unless a test proves
they are unnecessary.

Run the complete policy with `pnpm run lint`. Use `pnpm run lint:hooks` or
`pnpm run lint:unused` to isolate an audit category. Package-level scripts
delegate to these root-owned commands so every invocation uses the shared root
configuration.

The root `lint:ui` command covers the shared UI and visualization packages.
The Workspace Web gate owns browser product checks.
