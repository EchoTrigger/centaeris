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

Run the complete policy with `npm run lint`. Use `npm run lint:hooks` or
`npm run lint:unused` to isolate an audit category. Package-level scripts
delegate to these root-owned commands so every invocation uses the shared root
configuration.

## Current audit inventory (2026-09-04)

### Completed unused cleanup

- Removed the unused `assertOkResponse` helper from
  `packages/desktop/scripts/smoke-runtime.mjs`.
- Removed the unused `repoRoot` binding from
  `packages/desktop/scripts/smoke-window.mjs`.

`npm run lint:unused` reports no remaining findings in this repository.

### Hook dependency investigation

Completed ChatArea lifecycle investigations:

- Session hydration and scroll-follow behavior are covered by
  `chat-area-hydration-race.test.tsx` and `chat-scroll-follow.test.tsx` and now
  live in dedicated Hooks.
- Runtime configuration and context usage are covered by
  `chat-runtime-config-race.test.tsx` and `chat-context-usage-race.test.tsx` and
  now live in dedicated Hooks.
- Queued prompts, stop recovery, and single-dispatch behavior are covered by
  `chat-queued-prompt-lifecycle.test.tsx`. The test exposed a late stream-open
  callback that could revive a stopped run; stream identity is now checked
  before setting the running state. Queue ownership now lives in
  `useQueuedPromptLifecycle.ts`.
- Stream connection open/error state, all terminal outcomes, exactly-once
  terminal callbacks, foreign-run rejection, and callbacks arriving after
  closure or replacement are covered by `chat-queued-prompt-lifecycle.test.tsx`.
  The tests exposed a late payload that could rewrite a completed turn. Stream
  callbacks now verify both message and AgentRun identity before reducing a
  payload. Connection ownership lives in `useAgentStreamConnection.ts`, while
  terminal turn/session effects live in `useAgentStreamTerminalLifecycle.ts`.
- Session view cache writes are covered for debounce timing, coalescing, session
  ownership, active replay identity, and unmount cleanup in
  `chat-queued-prompt-lifecycle.test.tsx`. Timer ownership and latest-view refs
  now live in `useSessionViewCachePersistence.ts`.
- Required-question submission is covered for successful continuation, failed
  retry, rapid duplicate submission, and late responses after a session switch
  in `chat-queued-prompt-lifecycle.test.tsx`. The tests exposed duplicate RPCs
  before React could render the disabled state and an old-session response that
  could start a new stream. Submission locking and response ownership now live
  in `useQuestionCompletionLifecycle.ts`.
- Durable message-ID adoption and ordinary prompt response ownership are
  covered for atomic remapping, stream event routing, session switches,
  switch-away-and-back ABA, and newly created sessions in
  `chat-queued-prompt-lifecycle.test.tsx`. The tests exposed a late response
  that could remap the visible messages and start an old-session stream.
  Session epochs now live in `useSessionRequestOwnership.ts`; remapping lives in
  `useDurableTurnMessageIds.ts`.
- Stream event reduction is covered for duplicate event IDs, visibility
  filtering, canonical context refresh without polling, and fail-closed
  handling of unsupported payloads in `chat-queued-prompt-lifecycle.test.tsx`.
  The characterization tests found no behavior defect; the cohesive routing
  boundary now lives in `useAgentStreamEventLifecycle.ts` without tests locking
  that implementation choice in place.
- Prompt transactions and hydration now enter their controllers through five
  and six named semantic ports instead of long flat lifecycle argument lists.
  Ordering, request ownership, replay cancellation, unmount cleanup, duplicate
  stream starts, and callback identity are covered by behavior tests. A
  Profiler test also proves that a pure assistant text delta updates the store
  without committing the ChatArea root, so the last ChatArea source-shape
  assertion has been removed.
- Agent result rendering is split into pure transcript projection, tool
  activity, process, subagent, and final-answer boundaries. Render-count tests
  prove that final-answer deltas do not rerender stable process Markdown and
  process updates do not rerender a stable final answer. AgentResultStream
  source-shape assertions have been removed.
- App workspace, session, and panel lifecycles and ChatComposer prompt, MCP, and
  runtime-control lifecycles are covered through component behavior harnesses.
  Their previous source-shape assertions have been removed.

The current Hook audit contains seven warnings, all listed below. This refactor
introduced no new warnings and removed the four AgentResultStream findings.

| Area | Locations | Required evidence before a change |
| --- | --- | --- |
| Code preview lifecycle | `CodePreview.tsx:171`, `CodePreview.tsx:198` | Prove document contents, extensions, and target-line scrolling update correctly without recreating the editor unnecessarily. |
| Model selection reset | `ModelsDialog.tsx:237` | Prove provider/model changes preserve the intended selection and refresh behavior. |
| Session preview reload | `AgentSessionPreview.tsx:49` | Prove a reload revision causes exactly one refresh. |
| Composer reset | `ChatComposer.tsx:181` | Prove the panel reset key intentionally clears the expected composer state. |
| Virtual list follow mode | `VirtualMessageList.tsx:231` | Prove height growth follows the end only while follow mode is active and does not disturb a reader scrolled upward. |

The inventory records lint evidence only. It does not authorize a production
change and should be updated when a focused test resolves an item.
