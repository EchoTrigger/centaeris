# Centaeris CLI

The `centaeris-tui` Rust crate builds `cet`, the Ratatui terminal and headless
CLI. The TUI and `print`/`json` modes run the local Agent and tools in the `cet`
process. The `rpc` mode serves the Agent JSONL protocol over standard input
and output in that same process. All three entry points use the local app
service in [`centaeris-cli-runtime`](runtime/src/lib.rs).

Multiple TUI and `print`/`json` processes can share the local data directory.
Each root session and its descendants have one writer at a time. Desktop's
separate `centaeris-runtime` service and the raw `rpc` mode require exclusive
access to their data directory. The package
includes `cet.mjs`, the Node entry point under `@centaeris/tui-local`, and
the `dist/` output used by the Windows TUI archive.

See [Architecture](../../docs/architecture/Architecture.md) and
[Windows setup](../../docs/getting-started/Windows.md) for the build and
first-run paths.

## Slash commands

The TUI command menu contains `/model`, `/effort`, `/resume`, `/dashboard`, `/new`,
`/clear`, `/stop`, `/help`, and `/exit`.
`/help` opens this same menu. Enter activates its highlighted command; Tab
completes only the command name. Other slash command names return an unknown
command error without changing the active chat.

Use `/resume` to browse pinned and recent sessions grouped by workspace, with the
current workspace first. Press `/` or type to filter by title or session ID. Use Up/Down
to move from search to the results, then Enter or click a row to restore it.
Esc returns from search to the list, then clears the filter or closes the
picker. Open sessions keep their drafts, attachments, scroll position and
running tasks while you switch between them. A session opened for writing
stays owned by that terminal window until the window exits.

`/dashboard`, its alias `/sessions`, Ctrl+\ or Ctrl+4 opens the Dashboard
to preview and reply to open sessions.
The Inactive group loads saved sessions from the current working directory.

`/new` and `/clear` both open a blank session in the current workspace.
The previous session remains available in `/resume` and its task keeps running.

An occupied session opens its saved transcript in read-only mode. Press `R`
to retry opening it for writing. Press `F` to start a separate session with the
history before the latest verified user input that started a turn. That input
and its images appear in the new draft. Mid-turn supplements and background
continuations stay in the original session.

With a row selected, press Ctrl+R to rename it, then Enter to save or Esc
to cancel. Ctrl+T pins or unpins it. Press `d` to review deletion. Confirm with `y` or click
Delete. Press `n` or Esc to cancel. Confirmed deletion also applies to child
sessions and may stop runs. The picker refreshes after the Runtime reports
the deletion.

## Editing and pointer controls

The composer treats combined characters and emoji as whole graphemes. Shift
with the arrow keys extends a selection; Ctrl with Left/Right moves by word.
Ctrl-Z undoes an edit, and Ctrl-Y or Ctrl-Shift-Z redoes it. Image placeholders
are edited as whole attachments, including undo and redo. Sending or clearing
the draft ends its undo history.

Terminal input is normalized before commands and panels handle it. Detected
multiline pastes insert one undoable edit, preserve a trailing newline, and wait
for a separate Enter before sending a prompt or supplement. CRLF and CR become
LF. Windows terminal paste can arrive without a paste marker, so detection uses
rapid input batches. Absolute Windows drop paths are recognized within their
own batch and use the attachment handler independently of existing draft text.
An image inserts an atomic placeholder and a trailing space at the cursor; one
undo removes the whole insertion. Other unmarked single lines followed by Enter
retain ordinary typing and submission behavior.

Drag to select text in the composer or transcript, including tool output. Releasing
the mouse copies a nonempty selection to the native clipboard; double-click
selects a word and triple-click selects a source line. Display wrapping does
not add copied newlines. API key inputs remain masked and do not copy or keep
undo history. Clipboard errors appear in the interface.

The wheel scrolls the area under the pointer. Overflowing lists, transcript
and long drafts have draggable scrollbars. Dragging a selection
beyond the composer or transcript scrolls the loaded text. Active panels own
their pointer input; resizing or changing panels ends a drag. Click an image
placeholder to open the draft image viewer. Compatible Kitty and iTerm2
terminals support pixel previews with wheel zoom, drag to pan, and `0` to fit.
The text preview shows image dimensions, format, byte size, and the attachment
path. Press Esc to return to the draft. Tool images stay with their results in
the transcript. Kitty, Ghostty, and WezTerm can display them inline with the
Kitty graphics protocol. Windows native terminals and tmux use text previews.

## Terminal layout

The transcript separates prompts, answers, and tool output. Long tool commands
align their continuation lines with the command, and the current run status
sits above the composer. Failed commands and requests keep their error summary.
The borderless composer uses the terminal background and aligns its text with
the submitted prompts.

Markdown tables align cells by display width. In narrow windows, cells keep
their text and separators while logical rows wrap. Fenced code gets a separate
indent and retains the indentation of its contents.

## Model and reasoning selection

Use `/model` or Ctrl-M to open the model picker. Type to filter, use Up/Down
to select, and press Enter to continue to that model's reasoning levels.
Choose a level and press Enter again to apply the model and level together.
Esc returns from reasoning to models, then closes the picker without applying.
Tab switches providers on the model page. Models with no selectable levels
skip the reasoning step. The current catalog has no selectable context windows.

Completion stops at the command name. Tab can complete `/mo` to `/model `;
pressing Tab again does not fill a model or reasoning level. When the command
list is open, Enter activates the highlighted command. Selecting `/model` or
`/effort` with Enter or a click opens its selection page directly. Shift-Enter
and Alt-Enter still insert a newline. Mouse selection on the model or reasoning
page uses the same steps as Enter.

The navigation row uses an arrow and highlight. The active model or reasoning
level is marked with `(current)` after its name. Names and descriptions use
shared display-column widths so moving the selection keeps both columns aligned.

`/effort` opens the current model's reasoning picker. `/effort high` sets a
level directly; `/model provider/model high` selects both explicitly. The Host
validates and remembers model-specific choices. A setting change applies to
subsequent AgentRuns; a running AgentRun retains its admitted configuration.

## Build

```powershell
.\scripts\build-tui.ps1
```

Builds `cet.exe`, licenses, built-in System Skills,
and the package manifest before creating the Windows archive.

## Interactive and headless modes

`cet` and `cet --mode tui` open the existing terminal interface.
Use `--workspace <directory>` to select an existing TUI working directory;
otherwise the current directory is used. `cet --help` and `cet --version`
return without opening a terminal interface or starting the Agent runtime.
The other modes use the same Agent protocol, Sessions and Run lifecycle.

```sh
cet --mode print --cwd . "Explain this project"
cet --mode json --session session-id "Continue the analysis"
cet --mode rpc
```

Print waits for a committed Run terminal event, reads the durable Run and
assistant projections, and writes the complete final assistant text.
JSON emits an `accepted` record, canonical `session/update` notifications,
and a `completed` record containing the durable Run and `outputText`.
Transport and request errors emit an `error` record.

`--session` continues a Session using its existing working directory.
`--cwd` selects the working directory for a new Session in print or JSON mode.
These options are mutually exclusive; `--workspace` belongs to TUI mode.
Use `--` before prompt text that begins with a dash.

Ctrl-C requests cancellation and waits for the durable outcome. Exit codes
are 0 for success, 130 for cancellation, and 1 for failure, a stopped Run,
or a CLI error.

RPC serves JSON-RPC requests and asynchronous notifications over standard
input and output. Clients initialize the connection, correlate request IDs,
and wait for committed Run outcomes. `cet ssh connect` uses the system
OpenSSH client and inherits the terminal.
