# Troubleshooting

## Git Bash is unavailable

Windows host execution requires a verified Git for Windows Bash. Check that Git
for Windows is installed and that `bash.exe` is discoverable, following the
[local execution setup](architecture/LocalExecution.md). A missing or unverified
Bash fails explicitly instead of running an unsandboxed fallback.

## CLI build is stale

Rebuild `cet.exe` from the desired revision. The Runtime is embedded in the CLI.

## System Skills are missing

Rebuild the TUI distribution from a checkout containing
`system-skills/`, then restart the Host. The release build validates and bundles
these Skills with their LICENSE and NOTICE files. For a standalone development
Runtime, set `CENTAERIS_SYSTEM_SKILLS_SOURCE` to the checkout's
`system-skills/` directory. If the installed System Skills directory is damaged,
preserve it for diagnosis before using the Host's supported repair path.

## A Plugin cannot be activated

Inspect the package error for a missing resource, stale digest, duplicate
identity, invalid credential reference, unsupported contribution, or live MCP
contract mismatch. Activation errors are isolated to the affected package and
are not repaired by weakening the public schema.

## Local state is corrupt

Do not delete `~/.centaeris` as a first troubleshooting step. Preserve a copy,
identify the failing file or database contract, and use the explicit recovery
path exposed by the Host. Removing the data root deletes local Sessions,
configuration, credentials, Plugins, and Skills.
