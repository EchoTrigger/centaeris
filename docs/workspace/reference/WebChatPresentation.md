# Web chat presentation

Applies to the transcript, composer and source preview. Shared desktop
`typography.css` remains unchanged. Body, metadata and code values live in
`chat-typography.css` and match Desktop; Web-specific element mappings live in
`transcript-theme.css`. UI control and feedback roles are defined in
`docs/development/FrontendFoundations.md`.

## Type roles

| Role | Size / line height | Examples |
| --- | --- | --- |
| Body | 14 / 24 px | Messages, answers, expanded reasoning, document prose |
| Metadata | 12 / 18 px | Work status, duration, tool headings, collapsed summaries, source links |
| Code | 13 / 21 px | Inline/fenced code, commands, raw tool output |

Markdown headings retain their existing hierarchy. Ordinary prose tables inherit
body size; code remains monospaced. Error colors retain their semantic meaning.

## Motion inventory

| Interaction | Duration | Rule |
| --- | --- | --- |
| Hover, disclosure arrows | 140 ms | Color/background or rotation only |
| Thinking, tools, work process | 180 ms | Height and opacity; reverse from current position |
| Source preview panel | 240 ms | Motion drives the split width, or the height when stacked |
| Preview ready content | 150 ms | Fade once when ready content mounts |
| Sent-message positioning | Existing 320 ms | User scroll interrupts; existing controller unchanged |
| Streaming prose | Existing at most 120 ms per burst | Durable/history content is not replayed |
| Thinking shimmer | Existing single 4 s pass | Ongoing state remains conveyed by label and elapsed time |

Disclosure initial mounts do not animate. Content mounts only on first opening,
remains during closure, becomes inert immediately, and unmounts on completion.
Reversal cancels the previous completion so it cannot remove reopened content.
The source panel retains an empty inert shell during exit; file content is
released immediately. The header spans both panes. An open preview keeps its
chosen width, and direct resizing updates the split without a tween. While
reading earlier messages, the transcript anchors a visible paragraph before the
workbench changes size. The existing disclosure and recovery anchors take
precedence, and following readers remain on the latest content.
Reduced motion settles disclosure and panel geometry immediately; existing
shimmer and send-scroll reductions remain in effect.

## Automatic folding

Within a mounted Session turn, process starts open. The first final answer folds
it unless the user has already opened a process detail. After deliberate opening,
stream updates and temporary answer disappearance/reappearance cannot fold it
again. The user can still close it manually. A new turn or Session has its own
state; reopening a historical turn defaults to folded. This is local presentation
state, not a persisted Runtime or transport field.

Only turns with process content expose the completed work disclosure. A final-only
response retains its elapsed-time label as static text, with no disclosure body,
arrow or divider. Live reasoning also counts as process content. Work headers use
no divider in either state.

## Focused verification

From the repository root:

```powershell
node --test packages/workspace/web/tests/unit/work-progress.test.mjs packages/workspace/web/tests/unit/message-scroll.test.ts
$env:CHROME_BIN = 'C:/Program Files/Google/Chrome/Application/chrome.exe'
node --test packages/workspace/web/tests/browser/chat-presentation.test.mjs
```

The browser fixture uses real React components and computed CSS in an isolated
Chromium profile, with a loopback Vite server and synthetic content. It checks
both ordinary and reduced motion, lazy mount/cleanup, interrupted reversal,
history defaults, user choices across refresh, preview cleanup and type roles.
It needs no account, backend, model calls or performance workload. `CHROME_BIN`
can point to another installed Chromium executable. Lint, Web typecheck and Vite
build remain the normal source validation gates.
