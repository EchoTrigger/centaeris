# Execution environment network policy

Workspace owners and administrators set the default network policy in Settings →
Execution environment. The authenticated, CSRF-protected GET/PATCH endpoint is
`/api/workspaces/{workspaceId}/environment-settings`, with the exact response and
update body `{"network":"disabled"}` or `{"network":"publicInternet"}`.
Other members cannot read or change these administrative settings.

The default is disabled. Creating a Session snapshots the current workspace default
into its SessionEnvironment. Updating the default affects new Sessions only;
existing environments, queued Runs and signed grants retain their policy. The
database enforces the environment snapshot against direct and bulk updates.

Benchmark task binding is the sole pre-execution override: the existing maintenance
binding sets the same environment network snapshot from the immutable task profile,
before any Run or Session event exists. A benchmark profile and environment policy
must agree. This uses the same signed policy and Docker network selection as ordinary
execution. Task-specific images, working directories and writable root filesystems
remain explicit benchmark capabilities.

New grants use `workspace.agent_run_authorization.v3`, requiring the top-level
`network` field and a distinct v3 signature domain. Ordinary execution keeps its
restricted filesystem and user identity when internet access is enabled. Docker
uses `none` for disabled and `bridge` for publicInternet; configured execution
proxy settings apply only to enabled environments. This switch controls connectivity;
Docker bridge alone does not implement an address-level private-network firewall.
Container adoption checks the authorized network mode before reuse.

Upgrade Runtime and API together before enabling the setting. Migration
`0021_environment_network` initializes ordinary historical environments as disabled
and reconstructs benchmark network values from their authoritative bound profiles.
Existing v1/v2 grant payloads, digests and signatures remain byte-for-byte unchanged.
Readers reconstruct v1 as disabled and v2 from its signed profile so pending work
and history remain valid. New issuance always uses v3; no legacy field aliases are
accepted. Reversal is blocked once v3 grants or ordinary internet policy facts exist.

Acceptance requires API permission/snapshot/signature tests, historical migration
and direct-write tests against disposable PostgreSQL, cross-language v3 vectors,
and Runtime create/adoption tests for both modes. Synthetic Docker transport tests
do not establish deployed network reachability or isolation.
