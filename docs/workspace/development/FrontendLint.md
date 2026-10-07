# Frontend lint policy

The Web lint gate blocks deterministic defects and regressions against the
audited zero-warning baseline. `scripts/ci.ps1` runs the repository lint gate
before type-checking and browser tests.

## Blocking rules

Unreachable code, unused imports and variables, missing Hook dependencies,
Hooks called outside the top level, debugger statements, duplicate object keys,
duplicate parameters, and explicit `any` annotations are errors.

Use `npm run lint:hooks` and `npm run lint:unused` when investigating one
category. The Web package scripts delegate to the repository root so every
invocation uses the shared root configuration.

Never apply unsafe lint fixes to dependency arrays. A production Hook change
requires a focused failing test for the suspected stale closure, missed reload,
duplicate listener, connection churn, or rendering regression. A dependency
used intentionally as a reset or refresh signal must carry a narrow inline
explanation and behavior coverage before the diagnostic is suppressed.

## Current audit inventory (2026-09-04)

### Zero-warning baseline

`npm run lint`, `npm run lint:hooks`, and `npm run lint:unused` report no
findings. The global CI lint step now owns that invariant; tests do not pin
diagnostics to individual source line numbers.

### Completed unused cleanup

- Removed the unused file-preview catch binding from `AppRoute.jsx`.
- Removed the unused `selectedItem` binding from `LibraryRoute.jsx`.
- Removed the unused `Trash2` import from `ModelSettings.jsx`.

`npm run lint:unused` reports no remaining findings in this repository.

### Core chat type-checking

Compiler inspection corrected one part of the original finding: with
`allowJs: true`, `include: ["src"]` does collect `.mjs` files. Their function
bodies are nevertheless not checked while `checkJs` is false. A diagnostic run
with `checkJs` enabled found 155 strict diagnostics in the four original core
chat modules, so enabling it globally is not a safe one-line change.

The focused migration keeps the normal TypeScript gate green:

- `sessionEvents.ts`, `chatViewStore.ts`, `workspaceChatController.ts`, and
  `workspaceWebTransport.ts` are now strict TypeScript;
- their stream boundary uses explicit `StreamEntry` types and `unknown` for
  unvalidated event payloads;
- explicit `any` is a blocking lint diagnostic with a configuration test;
- a configuration test requires all four core implementations to remain `.ts`
  and keeps `strict` plus `noEmit` enabled.

Each conversion must preserve the existing unit and browser behavior tests. Do
not weaken strictness or introduce explicit `any` when adjacent chat modules are
migrated later. The original four-module core type-checking gap is now closed.

### Hook dependency audit

The audit characterized effects for chat telemetry, preview listeners, route
scope, retained streams, library loading, model loading, credential refresh,
search selection, and virtual-list follow behavior. Demonstrated stale-state
and route-scope failures were corrected. Intentional reset and refresh triggers
remain explicit at their owning effects with inline explanations.

The result is enforced globally: a new unsuppressed exhaustive-dependency
diagnostic fails both `npm run lint` and local CI. Similar-looking effects remain
separate when they have different owners, lifecycles, cleanup, or failure
handling.
