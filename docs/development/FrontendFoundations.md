# Frontend foundations

Desktop and Web should use upstream implementations for common frontend
capabilities so product work can concentrate on agent execution, useful views
and delivery. Desktop is the integration target first. Web adopts the verified
foundation only after maintainer acceptance of the complete Desktop candidate.
The accepted compact light visualization reference is the visual baseline.

## Capability ownership

| Capability | Implementation | Product-owned boundary |
| --- | --- | --- |
| Components and visual language | shadcn/ui Base UI + Nova | Theme tokens, business composition and platform chrome |
| Icons | One exact Lucide version | Brand marks and domain-specific illustrations |
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

`packages/ui/src/components/ui` contains the upstream-derived Nova primitives.
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

The application bar keeps the theme control visible at the top left, including
when navigation is collapsed. The workspace switcher shows the selected name and
path, offers recent workspaces, and separates changing workspace from expanding
its conversation group. Existing conversations show their title and own workspace;
New chat uses a welcome surface with workspace actions and no conversation title.

Sidebar row menus use explicit rename/save/cancel and confirmed deletion. Preview
panes use keyboard-operated tabs, adjacent-tab focus after closing, an overflow
list, retained view state, and retry for failed reads. Resources remain available
from both the welcome surface and an existing conversation. The composer exposes
available commands without discarding an unrelated draft; questions, model
selection and MCP configuration use labelled controls with owned async receipts.

Settings provides Appearance and Model services. Connection editing, model tests,
custom model parameters and removals retain explicit saving, cancellation and
failure recovery. Plugins and Skills share list/detail navigation, source and
scope information, search, enablement and installation flows. Scheduled tasks
exposes the Host list and occurrence history; creating or changing schedules
continues through the agent. A page cannot imply a backend capability that the
Host does not provide.

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
Desktop lazily loads its Nova mapping and the accepted compact visual language.
Generic sequences follow vertical reading order; actual containment uses nested
surfaces. Labels can wrap in narrow panels. Icons use the same Lucide dependency
as other Desktop components. The preview uses the production Markdown and
renderer path, rather than example-specific presentation overrides.

Web keeps its default renderer until Desktop acceptance. TUI continues to use
Core text projection of the same portable content; React dependencies do not
enter the terminal renderer. See [Inline visualizations](InlineVisualizations.md)
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
