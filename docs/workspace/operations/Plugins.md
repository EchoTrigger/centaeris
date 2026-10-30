# Plugin lifecycle

## Package carrier

Workspace accepts Plugin ZIPs through separate platform and workspace
management APIs. Platform uploads require a superuser; workspace uploads require
the current workspace owner/admin or explicit platform resource authority.
See [Workspace resource administration](WorkspaceResourceAdministration.md) for
shared packages, local overrides, and the forward migration. ZIP is a transport
carrier, not the installed package identity. The archive may contain the Plugin
root directly or one wrapping directory and must contain
`.centaeris-plugin/plugin.json`.

The API rejects archives that exceed entry, file, expanded-size, or path
budgets; contain absolute or escaping paths; use links or special files; or
contain duplicate and case-folding-colliding paths. ZIP is the only v1 upload
carrier. The API does not execute `npm install` or package lifecycle scripts.

## Install and update

1. The API streams the upload into a bounded staging location.
2. Runtime/Core inspection validates manifest, resources, paths, contracts, and
   package digests.
3. The API generates the installed catalog from the staged directory.
4. A valid package is moved atomically into the Plugin volume.
5. The upload carrier and staging directory are removed.

Failure leaves the previous installed package and catalog unchanged. The API
does not retain a second ZIP or an unused checksum sidecar.

An update that would invalidate an active AgentRun is rejected until the run is
terminal. Installation alone does not enable the Plugin in every workspace.

## Workspace enablement

Enablement is stored per workspace. Each new AgentRun freezes enabled package
identities, resource digests, credential versions, and compatible Core
contracts. Enabling or disabling a package affects later runs and does not
rewrite Session history.

An invalid package is isolated from other catalog entries. Administrators must
still be able to disable an enabled package whose connection or contract fails.

## Credentials

Plugin declarations contain credential references, never secret values.
Platform operators manage shared credentials. Workspace owners/admins create,
rotate, or delete their workspace credentials through scoped management APIs;
local credentials override the shared reference only inside that workspace.
Management responses contain metadata and never existing plaintext, and
secrets do not enter AgentRun authorization, Session events, logs, prompts, or
the package volume.

## Uninstall

Platform uninstall checks active-run and enablement constraints, removes the
installed directory, and regenerates the catalog atomically. Workspace uninstall
removes its local installation and enablement while retaining immutable bytes
needed by existing signed references. It does not delete unrelated
packages, workspace data, Session history, or user files. Credentials have a
separate explicit deletion path.

## Release source

Private extension source and development credentials do not enter Workspace
images. A released ZIP must be reproducible from a clean extension checkout and
must preserve executable modes and required third-party LICENSE/NOTICE files.
