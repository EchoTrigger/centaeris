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
