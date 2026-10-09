# Centaeris MCP

`packages/mcp` (`centaeris-mcp`) is the lazy MCP (Model Context Protocol)
connection adapter. It owns connection lifecycle, tool discovery validation,
and call translation between Core's dynamic tool contracts and an external MCP
server's protocol.

Connections are established lazily and retried with a bounded cooldown on
retryable failures. Discovery results are validated against Core's tool
contract size and shape limits (`centaeris_core::tool::limits`) before a
Plugin-declared MCP server's tools are exposed to a model.

`packages/runtime` composes this adapter for the Local Runtime; `runtime_server`
composes an MCP catalog inspection path for the hosted execution boundary. This
package does not run a second Agent loop or redefine Core's extension
semantics.

See [Architecture](../../docs/architecture/Architecture.md) and the
[Plugin/Skill package spec](../../docs/reference/SkillPackageSpec.md) for the
declaration format MCP servers are activated from.
