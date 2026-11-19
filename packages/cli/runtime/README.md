# CLI Runtime

`centaeris-cli-runtime` owns local profile and storage assembly, provider
configuration, execution, session ownership, events and shutdown.
`cet` uses it for the TUI, print/JSON and stdio RPC. Core owns the shared
Agent and Session semantics.

See the [Local Runtime protocol](../../../docs/reference/RuntimeProtocol.md)
for the stdio interface and registry generation.
