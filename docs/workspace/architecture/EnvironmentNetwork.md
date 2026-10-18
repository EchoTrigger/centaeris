# Execution environment network policy

Workspace owners and administrators set the default in Settings → Execution
environment. The authenticated, CSRF-protected GET/PATCH endpoint is
`/api/workspaces/{workspaceId}/environment-settings`, with body
`{"network":"disabled"}` or `{"network":"publicInternet"}`. Other members
cannot read or change this administrative setting.

The default is disabled. Each new Session snapshots the workspace default.
Updating the setting affects new Sessions; existing environments and signed
grants retain their snapshot. A maintenance-authorized profile binding may set
the same network field before that environment has been used. Once the complete
profile is bound, PostgreSQL protects it against model, bulk and raw updates.

The current `workspace.agent_run_authorization.v1` signs the network value inside
`environmentProfile` and binds the complete `environmentProfileDigest`. Ordinary
and Harbor execution use this same policy. Enabling the network does not alter
the profile's user or the model tool's filesystem permissions.

Docker uses `none` for disabled and `bridge` for public Internet. Configured
execution proxies apply only to enabled environments. Container adoption checks
the authorized mode. Docker bridge provides connectivity, not an address-level
private-network firewall.

Migration `0021_environment_network` retains its historical reconstruction role;
`0023_unified_environment_profile` moves known persisted profile facts into the
current profile and preserves historical signatures. Live execution consumes the
current structure. Coordinated upgrades retain historical data without adding
runtime readers for each historical schema.

Acceptance covers settings permissions, immutable snapshots, Python/Rust signing
parity, PostgreSQL migration and direct-write protection, and Docker creation and
adoption under both network values. Host reachability is checked separately in
the deployment environment.
