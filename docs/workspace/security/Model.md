# Security model

This document describes implemented trust boundaries. It does not publish a
security-reporting channel.

## Browser and API

The browser is untrusted input. Django owns authentication, CSRF, workspace
membership, resource authorization, administrative roles, and credential
management. Browser clients receive masked metadata and authorized file or
event projections, never internal service tokens or decrypted credentials.

## Internal services

API, Runtime, and worker authenticate internal calls with a dedicated token.
AgentRun authorization is signed and binds the exact run, workspace, model,
execution profile, files, and Plugin activation. Internal network placement is
not a replacement for token and identity validation.

## Durable and transient data

PostgreSQL is the durable control and Runtime fact store. Storage volumes hold
file bytes. Redis contains bounded transient state and may be evicted without
becoming authorization or history truth.

Credential values are encrypted at rest by the API and resolved only at the
adapter that needs them. They must not enter logs, prompts, Plugin packages,
AgentRun authorization, Session events, or browser payloads.

## Execution

Agent tools run in temporary containers with a frozen image and resource
profile. Capabilities are dropped and mounts, work directory, UID/GID, network,
CPU, memory, PID, and temporary-space policy are explicit Host inputs.

Runtime controls Docker through the host socket. Compromise of Runtime therefore
crosses the container-control boundary; individual AgentRun restrictions do not
make the Runtime service unprivileged. Production hosts must restrict access to
that service and socket.

## Plugins

An installed Plugin is trusted executable content selected by an administrator.
Validation protects package structure and declared contracts; it is not proof
that arbitrary CLI, Hook, or MCP code is benign. Package source, provenance,
licenses, credentials, and reviewed capabilities remain release
responsibilities.

Each AgentRun freezes exact package bytes and contract identities. A package
cannot silently add tools after authorization, and one package failure must not
invalidate unrelated catalog entries.

## Operational boundary

The production Compose override expects an external HTTPS reverse proxy.
Backups, host access, TLS keys, monitoring, secret injection, incident response,
and a private vulnerability-reporting channel remain deployment responsibilities
until the project publishes an implemented process.
