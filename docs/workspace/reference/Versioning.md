# Product versions and source identity

First-party package versions remain 0.1.0 across the root Rust, npm and Python
metadata. Public schemas keep their existing explicit version identities.
Source identity is one full monorepo SHA from the actual tested checkout.

Desktop, TUI and Workspace have independent manual release entries and tag
namespaces: desktop-v, tui-v and workspace-v. Producing source or candidate
artifacts does not authorize publication. Publication is opt-in, requires main
and successful CI for the exact SHA, and retains complete corresponding source.
TUI installers select a stable release that actually contains their TUI asset,
skipping Workspace and Desktop releases and checking later API pages.

The original Core and Workspace SHAs and path mappings are historical provenance
in [the snapshot manifest](../source-snapshot/manifest.json); they are not a live
cross-repository dependency pin. The old Workspace repository remains unchanged.
