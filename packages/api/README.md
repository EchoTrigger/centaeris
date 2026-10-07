# Workspace API

The Django API is the hosted HTTP transport and product control plane. It owns
authentication, CSRF, workspace membership and access control, private Agent
configuration, model credentials, files, Plugin installation, AgentRun
authorization, product jobs, and durable product projections.

Core remains the owner of Session, model, tool, continuation, citation,
artifact, and Runtime job semantics. The browser connects only to Django REST
and SSE; it does not connect directly to Runtime, Storage, Docker, Redis, Agent
Memory, or model-provider credentials.

Repository-level boundaries are documented in:

- [Architecture](../../docs/architecture/Architecture.md)
- [Workspace API](../../docs/reference/API.md)
- [Configuration](../../docs/reference/Configuration.md)
- [Plugin lifecycle](../../docs/operations/Plugins.md)
- [Data and recovery](../../docs/operations/Data.md)
- [Release gate](../../docs/eval/ReleaseGate.md)

## Local development

Create the private `.env` from `.env.example` and use the root startup path for
the complete service graph:

```powershell
pwsh -File scripts/start-local.ps1
```

For an API-only development environment with authorized dependencies already
available:

```powershell
uv sync --locked --package api
uv run --frozen --package api python packages/api/manage.py migrate
uv run --frozen --package api python packages/api/manage.py bootstrap_superadmin
```

The required configuration and bootstrap constraints are defined once in
[Configuration](../../docs/reference/Configuration.md). Do not put usable
secrets in commands, documentation, images, or test results.

Compose runs the one-shot `api-init` job before API replicas. It validates the
installed Plugin catalog, applies migrations, and bootstraps the configured
administrator without clearing the Plugin volume. The persistent `gc` service
owns server-side expiry after durable trash deadlines.

## Focused gate

```powershell
uv run --frozen --package api python packages/api/manage.py test app_core.tests app_core.test_csrf app_core.test_bootstrap_superadmin app_core.test_workspace_invitations app_core.test_workspace_members app_core.test_workspace_groups app_core.test_source_permissions app_core.test_agents app_core.test_http_modernization app_core.test_trash_projection --settings=api.test_settings
```
