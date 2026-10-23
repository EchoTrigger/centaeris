# Frontend foundations

Desktop and Web should use upstream implementations for common frontend
capabilities so product work can concentrate on agent execution, useful views
and delivery. Desktop established the accepted foundation. Web now shares its
underlying primitives without adopting Desktop page or chat interaction policies.
The accepted compact light visualization reference remains the Desktop visual baseline.

## Shared package and host policy

`packages/frontend-foundations` owns the upstream-derived Nova components,
headless Base UI entry points, Motion and panel-library entry points, theme
declarations, the document-scoped theme store and platform-preference provider.
Consumers import explicit package subpaths. React remains a peer dependency;
host controllers own their business state and asynchronous request lifetimes.

Desktop's existing component modules forward to the shared Nova components.
Web composes the headless primitives with its existing CSS, labels and layout;
it does not import Desktop's Tailwind reset or palette overrides. Shared theme
declarations retain their existing values. Icon defaults remain 1.5 in Desktop
and 2 in Web. The provider respects the platform's reduced-motion preference.

Web retains its independent model and effort controls, confirmation flow,
centered/new-conversation input behavior, navigation, attachment flow and draft
ownership. Its transcript still folds process details on the first final answer
and respects manual reopening and closing. The disclosure component retains
Web's lazy mounting, reading-anchor behavior and animation timing while using
Motion mini underneath. Sharing components does not change Web's stream,
authority reconciliation, virtual-list controller or execution environment.

Real DOM tests protect menu selection, dismissal, focus, disabled states,
confirmation and drafts. Browser disclosure tests protect visible reading
position, close/reopen continuity and reduced motion. Both product gates check
the shared package; the Web image and standalone build install its workspace
dependency closure.

## Capability ownership

| Capability | Implementation | Product-owned boundary |
| --- | --- | --- |
| Components and visual language | shadcn/ui Base UI + Nova | Theme tokens, business composition and platform chrome |
| Icons | One exact Lucide version; Desktop uses LucideProvider with a 1.5 stroke width | Brand marks and domain-specific illustrations |
| Complex animation and frame scheduling | Motion | Reader lifecycle, scroll anchoring and output completeness |
| Resizable workspace | react-resizable-panels | Default layout, visibility, constraints and remembered user sizes |
| In-app sorting | dnd-kit React | Authoritative session order and asynchronous persistence |
| Shared reactive view state | Zustand vanilla stores and selectors | Scoped ownership, host adapters and domain projections |
| Inline visual content | json-render React | Core catalog, strict validation and portable text semantics |
| Large lists | Existing TanStack Virtual | Follow mode and transcript presentation |
| Editors and terminals | Existing CodeMirror and xterm | Host file and terminal adapters |
| Routing, localization and browser storage | Existing React Router, i18next and idb where used | Product routes, copy and persistent data contracts |

Exact dependency versions live in package manifests and the workspace lockfile.
Desktop uses the Base UI Nova registry (`base-nova`), rather than reusing the
former `new-york` setup. The CLI configuration identifies the selected registry;
changing this setting alone does not migrate existing components.

## Desktop integration

`packages/ui/src/components/ui` exposes the shared upstream-derived Nova primitives.
Business components compose these primitives. Focus trapping, keyboard selection,
outside dismissal and floating positioning belong to Base UI. The shadcn MIT
notice is preserved in `THIRD_PARTY_NOTICES.md`. Element resets stay in the CSS
base layer so they cannot override the generated component utilities. Simple
hover transitions and upstream popup CSS remain appropriate; Motion owns the
custom animated interactions and frame scheduling. Native View Transition owns
browser snapshots; Motion mini executes their clip animation. The pinned Motion
runtime supports `pseudoElement` but its option type omits it, so a narrow typed
options extension is protected by a real Motion-to-WAAPI integration test.

Workspace panels use library constraints and separators. Product code retains
symmetric size limits, a useful minimum width, equal-split reset, remembered
user resizing, and a compact single-view arrangement. Resizing across embedded
previews temporarily prevents iframes from consuming the pointer.

Pinned sessions use the library's pointer and keyboard sorting. Keyboard users
pick up with Space, move with arrow keys, save with Space and cancel with Escape.
A save includes the complete authoritative list, is not reentrant, and cannot
overwrite a catalog changed during a drag. Failed writes return to the
controller's reconciled order. Native file upload uses browser file interfaces.

## Desktop pages and interactions

Use a restrained tool interface: preserve familiar information structure and
icon meanings, show each fact once, and keep routine surfaces free of tutorial
copy. Instructions belong beside the action that needs them; failures, scope,
credential rules and destructive consequences remain explicit. Adopting mature
components improves interaction quality without introducing new entry points.

Settings categories occupy the existing application sidebar slot. The toolbar
Back and Forward controls own return navigation; settings has no separate Back
button. Hiding the sidebar preserves the active category and retained chat view.
Settings, model services and extensions share a centered 720px content column,
with responsive insets and left-aligned headings. Plugins and Skills retain their
upper-left selector in list and detail views. Skills starts at All sources and
groups Personal and System entries; plugin-owned skills remain under their plugin.
Management entry actions use Nova outline buttons; saving and confirmation use
the shared neutral solid-surface role. Cancel, refresh and return actions use
ghost buttons. Focus, links and explicit warning/error meanings keep their
existing semantic colors.

Theme selection lives in Settings → Appearance. The application bar keeps
navigation and application menus without a theme shortcut. Workspace headings expand and collapse the whole group; the separate menu owns rename and
removal. The selected state uses text emphasis rather than an additional check.
New chat uses a short local-time greeting and the same two-row ChatComposer as
an existing conversation. Startup restores the last workspace and loads the
session catalog without selecting a conversation. Workspace identity and input
position are independent:

| Trigger | Composer state | Position |
| --- | --- | --- |
| Startup or New chat | Welcome, with the remembered workspace | Center |
| Select or open a workspace | Preserve the current draft state | Unchanged |
| Type, edit, clear or submit a draft | Welcome, awaiting the first response | Center |
| First assistant text, reasoning, tool content or error | Conversation | Bottom |
| Edit or clear input after response begins | Conversation | Bottom |
| Explicitly select a conversation | Conversation | Bottom |

Draft submission and automatic session adoption keep the composer centered until
actual assistant content arrives. Motion supplies the non-bouncing spring
transition with a 0.6-second visual duration only for the first response; reduced
motion moves immediately. Explicit conversation selection places its composer at
the bottom directly and stops any previous response-docking animation.
New chat returns directly to the centered welcome position, including after a
management-page detour or while response docking is still in progress.

The workspace chooser sits outside the input's upper left and opens upward
throughout a new draft. It shows Choose workspace until selected, then the folder
name and the same menu for switching. The trigger sizes to its content, up to
320px including icons and padding, constrained by the composer on narrow windows.
Only names beyond that available width use ellipsis. The greeting contains no
workspace label.
Selection retains the draft and input node. Sending stays
unavailable until a workspace is selected; browsing or cancelling never creates
a session. The bottom toolbar places Add context on the left. The right side places
the context ring before the model menu and Send/Stop. The context popover opens
above its trigger with end alignment and automatic collision avoidance.
The model menu separates
model choices from an Effort submenu using an inset rule. Base UI reserves room
for that submenu to open to the right. Usage numbers appear only in context
details; the resting context control is an icon.

The transcript scroll viewport spans the main pane; horizontal padding keeps its
text column aligned with the composer. Its visible bottom stops above the
measured composer dock, so following and the return-to-latest button cannot
settle beneath input controls. Live row resize consumes ResizeObserver box data
and reports changed heights once instead of forcing layout for every notification.

Conversation entry and sent-turn anchoring keep following through deferred row
measurements. Scroll notifications from initial positioning, virtual-row
compensation or composer viewport resizing cannot change reading intent. Reserved
reply space is retained until the response consumes it or the reader takes control. Wheel, navigation keys, touch movement
and scrollbar interaction pause following before the viewport moves. A downward
gesture already at the real end leaves following enabled. Detail expansion also
pauses immediately.

The Web transcript view separates input intent from actual movement. Inert wheel,
touch or navigation-key input does not detach following. Actual upward movement
or a requested older-history page pauses it. Chromium can apply scrolling before
a passive wheel listener runs, so input timestamps do not overwrite the last
observed position; native scroll and programmatic positioning maintain that
baseline. Latest-answer visibility and read acknowledgements remain independent
of automatic following.

TanStack owns virtual measurements, ranges and stable row keys. One content-anchor
controller preserves reading position while older pages are inserted, including
insertion within an existing answer. It captures the visible content before the
DOM commit, rather than a scroll offset at request start. Native scrolling updates
the target position and moves it to currently visible content when the old point
leaves the viewport. Opening a disclosure selects its title as the reading point;
returning to latest releases history anchoring.

The controller owns size corrections until committed measurements and pending
content or visual renderers have settled. Deferred renderers expose their pending
layout through aria-busy. Initial row refs retain their estimated or cached size
until ResizeObserver reports the committed layout. TanStack's direct DOM updates
publish measured row positions and spacer height together before paint, preventing
new history or deferred visuals from overlapping the following turn. The same
geometry notification restores the reading anchor and schedules the latest
follower even when the visible range is unchanged. Ordinary virtual-row resize
corrections are applied after the new spacer height commits; outside an active
content anchor, only rows entirely above the viewport receive those corrections.
Retained referenced text is rendered synchronously from the bounded content
cache, so returning to a virtual row does not commit a loading placeholder.

Transcript overscan keeps a six-row minimum and expands by measured distance to
cover two viewports before and after the visible range, capped at sixty extra
rows on each side. This handles mixed short/long turns without mounting the
whole history. Referenced content readers share in-flight reads for the same
content range. Clearing the view invalidates those reads; a late completion
cannot erase the replacement request or repopulate the cache.

Conversation loading exposes one generic loading status. Internal projection,
replay and materialization stages are not UI copy. A selected view stays in the
loading state until its snapshot resolves, including the first render.

The first snapshot waits for the bounded transcript, active runs and pending
questions. Runtime configuration and context usage refresh independently after
that snapshot is applied; neither optional read can hide or clear the body.
Their results use the existing session-request ownership plus a per-view refresh
identity, so switching away and back cannot accept an older visit's response.
Metadata updates preserve newer canonical values and use the existing view-cache
persistence. Cached content remains visible during bounded incremental checks.

Live assistant text uses the existing Motion presentation queue as it arrives;
runtime confirmation never gates its visibility. Completion drains the same
mounted reader under the live per-frame budget; its 200ms catch-up target never
forces a large backlog to appear at once. Historical messages remain static. Event reduction keeps its
existing 128-payload and 4ms frame budgets, with no additional transport buffer.

Each live model text block retains one renderer identity and keyed section while
it becomes a process record or final answer. Archiving preserves its exact source
prefix and lets the existing Motion queue drain; it creates a separate identity
for the next reply, including equal wording. Process summaries are bounded
previews of the same reply and do not duplicate an available streamed source.
Tools and authoritative run status update immediately while pending text catches
up. Static history, corrections, errors and reduced-motion behavior retain their
existing immediate presentation. Reader state remains owned by the mounted turn,
without a global presentation cache or additional frame scheduler.

Chat rows reserve their right-hand status for a Lucide LoaderCircle during work,
or an attention dot for unread completion and questions requiring a reply.
Opening a chat acknowledges unread completion; answering clears question attention.
The existing foreground catalog discovery also reads Runtime activity, with at
most two background question lookups in flight. It has no separate timer,
transcript hydration, or retry loop; stale discovery cannot overwrite newer
foreground events or selection. Runtime facts remain authoritative.

Sidebar row menus use explicit rename/save/cancel and confirmed deletion. Preview
panes use keyboard-operated tabs, adjacent-tab focus after closing, an overflow
list, retained view state, and retry for failed reads. Resources remain available
from both the welcome surface and an existing conversation. Slash commands expose configuration in the current chat: model and reasoning
choices remain inline, while model services, Skills and Plugins reuse their
existing managers in a dialog. These commands do not navigate to Settings.
Questions and MCP configuration retain labelled controls and owned async receipts.

Settings provides Appearance and Model services. Connection editing, model tests,
custom model parameters and removals retain explicit saving, cancellation and
failure recovery. Plugins and Skills share list/detail navigation, source and
scope information, search, enablement and installation flows. Scheduled tasks
exposes the Host list and occurrence history; creating or changing schedules
continues through the agent. A page cannot imply a backend capability that the
Host does not provide.

The toolbar and navigation share one background without top or left dividers.
The main surface rounds only its left corners. Closing navigation removes its
entire width and focusable controls while retaining disclosure state; the window
toolbar stays fixed. Icon controls use accessible names without hover labels.
Application menus open below their trigger in window-content coordinates, use
explicit English labels and native editing roles, and contain no separators.
Workspace selection uses File → Open Folder (Ctrl+O) or the welcome input’s
Choose workspace menu. Both reuse the existing Host actions. With no workspace
selected, New chat does not create a directory or start a conversation.

The shared welcome and conversation composer reserves two text lines above its
existing toolbar, with a 112px minimum frame and unchanged automatic growth.
Short user bubbles fit their text and padding instead of forcing a minimum width.

## Color roles

Desktop maps shared theme roles through its foundation layer; Nova utilities,
transcript components and portable visuals consume those roles. Color indicates
purpose rather than the amount of nested content:

| Surface or meaning | Role |
| --- | --- |
| Main reading canvas | Surface color |
| Sidebar and actual containment | Surface variant |
| Selection and stronger interaction emphasis | Surface container and hover |
| Fenced code and expanded tool output | Transparent code surface |
| Inline code within prose | Subtle inline-code surface |
| Primary and supporting text | On-surface and process text |
| Diff, failures, warnings and keyboard focus | Existing semantic colors |

Code reading areas retain spacing, scrolling and copy controls without a separate
large filled panel. Chat and workspace Markdown previews use the same code role
in light and dark themes. CodeMirror retains its main-canvas background and
purposeful active-line/diff highlighting.

## State boundaries

Session catalogs, workspace catalogs and preview tabs use one Zustand store per
mounted controller. Saved preview scopes remain inside that owner. Theme stores
are scoped to a document; platform listeners connect for the first consumer and
disconnect after the last. The content-range cache uses Zustand for reactive
capacity snapshots while retaining its existing bounded reads and identity
rules. Existing chat selectors continue to use Zustand.

React component-local state and DOM references remain local. Request ownership,
abort handles and stale-response checks remain in host-facing controllers.
CodeMirror and xterm retain their internal state. Server authority, execution
cancellation and persistence contracts remain in Core and the existing hosts.
Migrating a subscription mechanism does not redefine those contracts.

## Inline visualizations

The built-in `inline-visualizations` System Skill teaches when and how to emit
portable relationships. Its reference is generated from Core with the browser
contracts; the ordinary root prompt keeps general policy. The System Skill bundle
is packaged with Desktop and discovered through the existing Runtime mechanism.

The shared adapter validates content before delegating presentation to a host.
Desktop and Web lazily load the same Nova mapping and accepted compact visual
language from `frontend-foundations`. Transport, message ownership and host
interaction stay outside that adapter.
Ordered explanations use a thin left guide and neutral circular nodes without progress
states or number circles. The centered reference resource chain keeps its arrows;
actual containment uses nested surfaces. Both clients inherit their application
surface, text, boundary and semantic status colors in both themes. Labels can
wrap in narrow panels. Icons use the same Lucide dependency
as other application components. The preview uses the production Markdown and
renderer path, rather than example-specific presentation overrides.

Web rendering consumes the portable answer content independently of skill
installation. Hosted deployments discover `inline-visualizations` only when
the skill is installed in the Runtime catalog and execution environment;
updating the Web container does not change either catalog or pinned environment.
TUI continues to use Core text projection of the same portable content; React
dependencies do not enter the terminal renderer. See [Inline visualizations](InlineVisualizations.md)
for the content contract and validation limits.

## Acceptance

Review and verify the complete Desktop candidate before maintainer visual
acceptance. Do not request acceptance after isolated primitive replacements.
Pure visual changes use manual inspection; behavior migrations use focused
characterization or regression coverage. Tests observe input, keyboard and
pointer interaction, focus, state transitions, async recovery and resource
cleanup rather than former source names or incidental pixel values.

Run Desktop application/test type checks, source lint, the complete UI suite
and a production renderer build. Validate affected Electron host and licensing
checks. Verify the Web default adapter still works without migrating Web UI.
Reconcile against the maintainer's refreshed baseline before merge. Preserve
the primary checkout and independently review changes within the candidate.

## Upstream references

- [shadcn styles and component bases](https://ui.shadcn.com/docs/changelog/2025-12-shadcn-create)
- [Base UI](https://base-ui.com/react/overview/quick-start)
- [Motion](https://motion.dev/docs/react)
- [react-resizable-panels](https://github.com/bvaughn/react-resizable-panels)
- [dnd-kit](https://github.com/clauderic/dnd-kit)
- [Zustand](https://github.com/pmndrs/zustand)

### Workspace tool tabs and conversation rhythm

Both sidebars share one Motion timing and easing definition. Right-pane visibility animates through the panel library’s public layout API; manual resizing remains immediate, while interruption and reduced-motion changes settle at the latest requested visibility.

The preview uses one tab strip. Its plus creates a New tab page backed by the existing per-owner Zustand store. Tool selection replaces that launcher or focuses an existing view; closed launchers reject late results. Opening the page does not read the terminal catalog or create a process. Existing terminal discovery is explicit, and a create retry preserves its operation identity within the authoritative service instance. Closing a terminal view still detaches without ending its process; ending it is a separate menu action.

xterm uses application color roles and the existing code font. Theme/font changes refit its existing buffer and connection through FitAddon. Conversation row padding belongs to the measured virtual row, rather than accumulated margins and invisible action-row height. Process details stay compact; completed answers have copy and optional authoritative completion time; eligible latest user inputs retain their existing rewrite transaction.
