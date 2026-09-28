# Frontend manual acceptance

Run this checklist against a production UI build before a release that changes
Desktop rendering, streaming, scrolling, theme behavior, or Electron host
routing. Record the tested commit, Windows version, display scale, and result.

## Desktop conversation

- Open a short and a long Session; confirm the newest content opens without an
  automatic chain of older-page requests.
- Scroll away from the bottom while live answer and reasoning text arrive;
  confirm the viewport stays detached. Return to the bottom and confirm follow
  resumes.
- Expand and collapse reasoning and tool activity before, during, and after
  completion; confirm settled content and ordering remain exact. Opening a detail
  pauses bottom following before its height changes: the clicked title stays in
  place and the body grows downward, in live and historical rows. Delayed output
  must not move that reading position. Jump to latest resumes following.
- Exercise a large tool output through its bounded detail reader; confirm the
  transcript stays responsive and does not inline the entire artifact.
- Switch Sessions while history or a preview is loading; confirm a late response
  cannot replace the newly selected Session.

- Body/reasoning/document prose remains 14px/24px, labels are 12px/18px,
  and code is 13px/21px. Process summaries and final text remain directly visible;
  there is no Work toggle, whole-turn divider or historical elapsed label.
- The active status sits after the latest content at the bottom of the run. Its
  order is spinner, status text, then the current task's elapsed time. Use h/m/s without padding or zero units. Switching to a newer parallel task
  changes the clock; returning to older active work uses its own original clock.
  Wording/progress/egg changes and row remounts do not restart the same task.
  Clocks are view-only, never written into transcript/session snapshots. Reloading
  begins a new observation clock; completed history never shows one.
- Waiting/authentication/provider failure states are explicit and stop the spinner;
  normal state and Tachikoma eggs never mask them. Reduced motion disables spinning.
  The timer must not rerender the process transcript once per second.
- Completion leaves open tool/reasoning details and their current reading state
  intact. Existing file-pane transitions remain independent of conversation status.
- Single tools show the actual path/command/query and open their output directly.
  Consecutive tools share the same card frame, including a single Bash tool.
  Every member title is present immediately (including more than 32 tools); there
  is no group-level disclosure or persisted elapsed label. Only a selected body
  mounts/reads content. A description must not replace the executed command.
- Reasoning shows only its compact label until opened, without a preview sentence
  or shimmer. Open reasoning remains 14px and retains its reading state.
- Tool output has a 240px maximum scroll viewport, with short output using its
  natural height. Headers and bodies share 12px inset. No repeated Bash heading,
  command body or success footer surrounds the output. Known machine framing is
  omitted; explicit paging and copying remain available.
- Only the currently running last turn uses normal document flow. Final text alone
  does not move it into virtual history; the running flag must settle. Completing
  a visible turn keeps the same keyed subtree and its open preview page/scroll.
  Stale running history does not become a resident tail.
- Prepending history preserves the visible offset. Resize and live tail growth
  notify the existing follow controller; they must not override detached reading.

Cross-client common typography tokens and font files are checked with
`node packages/ui/scripts/check-typography-parity.mjs <web-src>`.
Host selector mappings and stream cadence remain client-specific; the Desktop
stream-presentation tests protect its existing grapheme pacing and terminal drain.

## Appearance and accessibility

- The top-right List icon opens a lightweight workspace overview without a
  permanent text label. Open files, review changes, or a child Agent from it;
  Escape and clicking outside dismiss it. Reopening refreshes Git status and
  reports read failures instead of retaining a stale success.
- Files, the file tree, review and child Agents open as closable content tabs.
  File browsing starts with an empty preview on the left and a persistent tree
  on the right. The first file replaces the Files landing tab. Keep at most
  three retained file tabs plus one italic preview tab: further new files replace
  only the preview. Opening a retained file from the tree swaps its position with
  the preview and preserves both views. Clicking a tab only selects it, without
  reordering. Review and Agent tabs do not consume file slots. Closing a retained
  file makes room for another retained file before the preview slot.
  Reopening the same resource reuses its tab. Switching or hiding the pane keeps
  scroll position and Agent reading state; closing releases only that view.
  Arrow keys and Home/End select tabs; close controls are separate buttons.
  Rapid file selection and late reads cannot restore a replaced preview. The
  workspace and file-browser separators remain subtle in both themes.
- Review refreshes on demand and reads the selected file's diff. Switch files
  during a slow read: late results must not replace the selection. Binary or
  otherwise unavailable diffs retain their reason; truncated output is labeled.
- At 1440, 1024 and 760px window widths, pane controls remain reachable. When
  the workspace has insufficient width for chat and content, the open content
  pane fills it; collapsing returns to chat without losing the open tabs.
- Chat and the workspace pane initially share available width equally. Opening
  the left navigation shrinks both while retaining their ratio. Drag the subtle
  separator and verify both sides remain usable; release outside the handle and
  confirm resizing stops. Arrow keys adjust it, double-click or Enter resets to
  equal widths. Hiding/reopening either sidebar retains the chosen ratio during
  the app session. The separator is hidden in the narrow single-pane layout.

- Switch light/dark themes rapidly and reload; confirm the last choice persists,
  the title bar matches, and no intermediate theme wins later.
- Repeat with reduced motion enabled; confirm content is complete without paced
  animation or a view-transition sweep.
- Check reasoning/tool summary hover and keyboard focus at 100%, 150%, and 200%
  display scale; confirm text remains readable and controls do not jump.

## Desktop host boundary

- Open file, browser, terminal, and review surfaces from the Desktop UI and
  confirm only the intended trusted renderer receives the result.
- Close, reopen, and exit the Desktop app with an active Session; confirm the
  same AgentRun continues and tray/window state remains coherent. Reconnect a
  second client, catch up the transcript, and explicitly stop that run.
- After pressing Stop, retain the active display until an authoritative
  terminal arrives. A cancellation receipt alone must not mark work completed.
  Connection loss must not fabricate success or cancellation.

## TUI process and compact output

- Reasoning stays hidden. Active tools render in source order with their actual
  path/command and automatic gray `└` output. There are no Work containers,
  tool disclosures, toggle arrows, tool focus or expansion shortcuts.
- Output shows at most six physical terminal rows, then one ellipsis row if text
  remains. Short output uses its actual height. No inner scrolling, paging,
  duplicate title, frame, metadata or help footer appears.
- Referenced previews load only visible tools, with at most four pending reads
  and one bounded 16 KiB range per preview. Identity and UTF-8 bounds are checked;
  changing session or projection invalidates stale results.
- Status and output use neutral gray. Failure, denial and interruption remain
  explicit in text. No success dot or bold tool title is used.
- Wheel and navigation scroll the main transcript. Automatic preview layout
  preserves following or detached reading; Ctrl+End resumes following. Input,
  text selection and copying remain usable.
- Known readable body ranges omit machine framing. Unknown output is preserved.

## TUI AgentRun completion

- Core-owned AgentRun identity and terminal facts control process visibility.
  Successful completion replaces execution details with gray structural lines;
  stage summaries and final answers remain visible. There is no reopening control.
- Final text alone does not close a running AgentRun. Completion without final
  text still leaves a structural marker. Failure/interruption retain results,
  status and reason. Active runs show working/waiting state.
- Historical pages missing presentation facts reconstruct them from authoritative
  source records read-only. No stored logs or projections are rewritten. Unknown
  terminal state is not guessed and does not cause an indefinite running spinner.
- Child completion/waiting cannot change the parent state. Pagination and replay
  preserve run ownership, stage summaries and final answers.
- Prepending history while text streams preserves older messages when the current
  answer is replaced. Main scrolling/following behavior is unchanged.

## New-case transcript regression acceptance

Use newly created sessions; no repair of existing stored records is required.

- Desktop groups consecutive same-family history tools, preserving intervening text
  and reasoning order. Appending a tool and virtual unmount/remount retain disclosure.
- A collapsed reasoning/tool record does not fetch its full referenced content.
  Long text and tool output page on demand; copy output copies the displayed page.
- Tool detail text has the same left inset as its title. History process records
  have compact spacing; final text retains a separate visual boundary.
- Desktop final answer arrival retains process content and open details. Reading
  older content must not be pulled to the bottom.
- Directory reads use directory summaries; absent file line metadata must not
  appear as invented line-one coverage or an unknown total.

### Desktop Git Review

- Open Review from the workspace overview, including in a newly initialized repository.
- Switch Staged and Unstaged sources. Unstaged includes new and conflicted files.
  A partly staged file appears in both sources with its corresponding diff.
- Stage and unstage individual files, including spaces/non-ASCII names, deletions,
  renames and a first commit. Unstaging must preserve working files and edits.
- Review must not show a commit-message form or a commit button. Commit through the
  agent's Git/gh workflow, then refresh to inspect the result.
- Change the index or HEAD using another Git process after opening Review. A write
  with the old snapshot must ask for a refresh. This does not promise exclusion of
  external writes racing after the preflight check.
- Resolve conflicts or finish merge/rebase in Git CLI; Review must not accidentally
  complete those operations. Refresh after returning to Desktop.
- Check light/dark themes and narrow windows: file actions remain usable, long
  paths truncate, and the list and diff can be scrolled.

### Git view navigation

- Switch Unstaged/Staged/Branch sources. Unstaged includes new/conflicted files;
  the same partly staged path has different stats and patches in the two sources.
- Enter a base reference for Branch and apply Compare (or Enter). Verify only
  committed changes since the common ancestor appear; no fetch is triggered.
- Start with collapsed file rows, verify +/− counts and rename paths, expand one
  diff and switch/collapse it. Late reads must not replace the current selection.
- Filter paths; search within the expanded diff and move between matching lines.
  Truncated patches explicitly limit the searchable content to the preview.
- Open a file from the expanded row, copy relative/absolute paths, and open the
  file menu for Stage/Unstage. There is no commit form or branch-mode write action.
- Change files using the agent or Git CLI, then return focus or wait 60 seconds
  with Review visible. Verify refreshed state; hidden views must not poll.
- Check light/dark themes, narrow panes and large diffs: paths truncate, headers
  stay visible while scrolling and menus do not push the diff off screen.

## Terminal transcript and Markdown surfaces

- TUI user messages retain `│`; assistant messages and tool headings use `●`.
  Streaming chunks, redraw, and narrow-width continuation lines must not repeat
  the marker. Separate top-level messages with a blank row; keep tool output compact.
- Inline code uses green foreground without a background rectangle. Tool actions
  are bold, with conservative lexical accents for strings, options, variables,
  and operators (not a shell grammar). Preserve command text exactly.
- Tool output starts with `  └ ` and subsequent rows use four spaces. Running,
  successful, and unsuccessful operations retain explicit text as well as color.
- Desktop inline code and fenced code keep their background and rounded corners,
  without outlines, in both chat and the workspace Markdown preview. Check light
  and dark themes; input and keyboard-focus outlines remain visible.


## Background process observers (before embedded terminal)

Use the same Session in Desktop and TUI. From TUI run
`/process start printf 'ready\n'; sleep 30`; this explicitly creates a
Runtime-owned Bash process with closed stdin and no execution deadline. The
ordinary model `bash` tool is unchanged and its unmanaged background children
are not included in this list.

- Open Desktop workspace overview > Task (ListCollapse). The entry appears only
  after the selected Session has retained process records; empty/unknown Sessions
  have no Task entry. It opens a session-bound Task tab; opening it again focuses that tab. Switching conversation does not
  retarget an existing tab. Deleting its Session closes its Tasks tab.
- Select a process to read its output; list refreshes every 1.5 seconds. Verify
  errors, nonzero exit, timeout, stopping and unconfirmed cleanup are visible.
  Stop targets only that process and reflects the server's observed state.
- Close/collapse the tab or disconnect a client. The process keeps running.
  Reopen the tab to read retained output. No terminal emulator or input is present.
- Generate more output than the Runtime retains: missing-prefix and local display
  truncation indicators must be visible. Binary bytes decode with replacement;
  split UTF-8 is decoded incrementally, independently for stdout/stderr. Each
  Desktop output view retains at most 256 Ki characters and renders plain text.
- TUI `/process` lists processes; `/process output <processSessionId>` opens a
  bounded, scrollable output viewer. Use arrows, PgUp/PgDn, Home/End or mouse
  wheel to scroll, `n` to read the next page, `r` to reread retained output and
  Esc to close. Esc/Ctrl+C in this viewer close the observer only.
  `/process stop <processSessionId>` explicitly stops a process. The viewer
  retains the latest 256 KiB; gaps are marked and each page has a cursor.
- TUI process requests are asynchronous. Closing a pending output request must
  not reopen the viewer when the reply arrives. Starting is never automatically
  retried; an uncertain start can be reconciled by listing the Session's processes.


### Active conversation workspace-switch regression

While an AgentRun is streaming, switch from its workspace to another workspace,
then return to the original conversation. The live replay can contain canonical
`runtime_event` text/tool events alongside `session_event` lifecycle events.
History recovery must accept both, preserve in-flight tool/text state, and avoid
adding duplicate text when the live subscription replays the same event IDs.
Unknown stream envelope types must still fail explicitly.

### Agent-owned background command completion

- Ask the Agent to use `process_start` for a short command. Confirm its record
  appears in Task with output and Stop, and that the Agent can continue working.
- Let it finish while the Agent is busy. The completion is queued until the
  Session is idle, then one visibly labelled Runtime automatic message and a
  follow-up Agent response appear. The Agent can read output using `process_read`.
- Close Task, switch workspace, or disconnect Desktop/TUI before completion.
  Delivery still occurs in the original Session. Returning restores the follow-up;
  the other workspace's transcript must not receive it.
- Cancel the originating Agent before completion. The process remains separately
  controllable; its completion must not restart the cancelled Agent. A manual
  TUI `/process start` also must not trigger automatic model work.
- Stop the process explicitly and distinguish its stop reason from natural exit
  and timeout. Output must be drained before completion is delivered. Runtime
  restart must never re-execute a command whose start outcome is uncertain.


## Desktop application navigation

- The title bar exposes Back/Forward, sidebar toggle and native File/Edit/View/Help menus. Windows caption buttons remain system-owned.
- File > Open Folder (Ctrl+O) uses the system directory picker. Cancel preserves the current session and panel; success opens a new-chat context for the selected directory without changing any existing session cwd.
- The sidebar shows New chat, Scheduled tasks and Plugins above cross-workspace Pinned/Recents, with Settings at the bottom. Collapse retains icon navigation; reduced motion disables the width animation.
- Pin/unpin persists through existing session metadata. Rename and deletion retain their previous inline behavior.
- Navigate between a chat and management pages, then Back/Forward. The active chat remains mounted on management pages; returning to another chat restores that chat's saved workspace tabs. Pending file previews explicitly offer retry on return.
- Plugins and Skill use keyboard-operable tabs on one page. Settings currently contains the existing model manager only; further categories remain a separate design task.
- Scheduled tasks reads the existing Host list/history and links occurrences to their sessions. Creating and changing schedules remains agent-driven; the page does not create schedules automatically.
- Browser-only preview cannot open native menus, folders or Host resources. Validate those in Electron.

### Plugin and Skill resource pages

- Open Plugins from the sidebar. Plugins / Skill navigation stays in the upper left; the list uses two flat columns, collapsing to one in a narrow window.
- Search plugins, scroll the list, then open one. Verify the full detail page and Plugins breadcrumb; breadcrumb return and global Back retain the list search and scroll. Global Forward restores the selected plugin.
- Verify existing install, reload, enable/disable, reveal, and managed-plugin removal actions against the local Host.
- In Skill, verify only Personal / System filters are shown and Personal is selected initially. Personal includes user and current-workspace skills; System includes only built-in skills. Plugin-owned skills remain in their plugin detail, not either Skill filter. Search by name or description. Open a workspace skill and verify its detail says Current workspace only. Skill Markdown scrolls inside the modal. Close or Escape returns focus to the row without clearing the search/filter or moving the list.
- Reload skills, open location management, inspect/toggle a source, and add a location using the native picker. Removing a user/workspace location still requires confirmation and leaves files unchanged.
- Confirm no store, connected-account, or Try now controls appear without supporting functionality.

### Model services settings

- Settings opens a single-column list of configured services, without a nested provider/model tree or global Save/Cancel footer.
- Open a service and use the Model services breadcrumb to return. Confirm connection details distinguish stored credentials from environment credentials, without claiming an untested connection works.
- Edit a connection, change its name or URL, and cancel. Reopen the editor and verify saved values remain unchanged.
- Add a built-in service through the searchable picker; add a custom service with its URL, protocol, and optional key. If configuration saves but credential storage fails, retain the editor/key for retry and show the partial-save result.
- Search models and test an individual model directly from the list. Verify its result names the tested model. Open a custom model to edit parameters in a modal; cancel discards changes and Save persists them.
- Remove a custom model or service through its controls and confirm the change persists without a separate global Save. Stored credential removal remains in the service actions menu.
- Check narrow layout, light/dark themes, native modal focus and Escape behavior in Desktop.

### Right-panel interactive terminals

- With the panel closed, use the header + menu; with the panel open, use its tab-strip +. Files/Review reuse tabs, Terminal opens a new numbered shell tab.
- Verify Windows chooses pwsh when installed and opens the selected workspace. Unix must be accepted separately with the configured login shell.
- Type commands, paste text, use completion/arrows/Ctrl+C, and run an interactive full-screen program. Resize the split pane and hide/show the panel; no zero-sized geometry or new shell should appear.
- Close a running terminal tab, switch workspace and return, then reopen Existing terminals. Verify the original shell remains and output replays; history beyond the bounded cache reports a gap.
- End terminal stops its process tree. Exited output remains readable; create another terminal for a new shell. Disconnect/reconnect must never resend user input or recreate old process identities.
