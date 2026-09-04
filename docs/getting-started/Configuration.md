# Local configuration

## User data

The Local Runtime resolves its data root from `CENTAERIS_DESKTOP_DATA_DIR` when
explicitly set; otherwise it uses `%USERPROFILE%\.centaeris` on Windows or
`$HOME/.centaeris` elsewhere.

The root contains configuration, Session files, Runtime state, encrypted or
protected credential material, managed Plugins, and installed System Skills.
Back up or remove it only as an intentional user-data operation. Uninstalling a
binary does not imply deleting this directory.

## Working directory

Each Session uses an explicitly activated working directory. Desktop can use a
selected directory or create a dated directory below the user's home for its
default-directory option. TUI starts from the current directory unless the
user selects another workspace.

Core treats execution paths as Host-owned identities. It does not copy the
working tree into a managed snapshot.

## Models and credentials

Model configuration is managed through the client and Local Runtime. Secrets
must not be placed in a Plugin manifest, Skill, Session, or source repository.
A missing or invalid credential prevents the affected model from running.

## Plugins

Managed Plugins live below the user data root. The executable may also contain
a read-only bundled Plugin directory. Duplicate package identities across roots
fail; search order never silently overrides a package.

Enabling, disabling, installing, updating, or uninstalling a Plugin affects a
later resolved activation. A running AgentRun keeps the activation it started
with.

## System Skills

Builds may receive a System Skills bundle through
`CENTAERIS_SYSTEM_SKILLS_SOURCE`. The build validates the bundle and preserves
required upstream LICENSE/NOTICE files. An empty bundle is supported.
