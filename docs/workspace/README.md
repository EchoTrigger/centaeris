# Workspace documentation

This index separates product architecture, transport, operations, security
boundaries, and release verification.

## Architecture and reference

- [Architecture](architecture/Architecture.md): component ownership and request
  flow.
- [Artifact object storage](architecture/ArtifactObjectStorage.md): storage coupling,
  proposed fixed-content contract and isolated RustFS validation.
- [Pre-admission cancellation](architecture/PreAdmissionCancellation.md): durable
  cancellation receipts without fabricated Session history.
- [Generated transcript contract](../packages/api/app_core/generated/README.md):
  Core-derived wire structure and Python validation.
- [Platform materials and MCP](architecture/PlatformMaterials.md): material
  access, durable processing, evidence receipts and citation presentation.
- [Versioning](reference/Versioning.md): product versions, Core revision, and contract identities.
- [API](reference/API.md): REST, SSE, and internal service contracts.
- [Web chat presentation](reference/WebChatPresentation.md): typography roles,
  motion inventory, disclosure rules and focused browser checks.
- [Configuration](reference/Configuration.md): environment variables and secret
  boundaries.

## Development

- [Core dependency](development/CoreDependency.md): portable checkout, exact Git pins and local co-development.

## Operations

- [Deployment](operations/Deployment.md): build, initialization, health, update,
  and production topology.
- [Data](operations/Data.md): volumes, persistence, deletion, backup, and
  recovery boundaries.
- [Plugins](operations/Plugins.md): ZIP upload through uninstall.
- [Document processing](operations/DocumentProcessing.md): required images,
  bounded streaming, and current measurement limits.
- [Office preview](operations/OfficePreview.md): LibreOffice-derived read-only
  DOCX, XLSX and PPTX viewing.

## Security and release

- [Security model](security/Model.md): trust boundaries, credentials, execution,
  and Plugin risk.
- [Memory acceptance](eval/MemoryAcceptance.md): offline discovery and private
  persistence checks, plus bounded real-model follow-up.
- [Release gate](eval/ReleaseGate.md): required source and deployment checks.
- [Performance evaluation](eval/PerformanceEvaluation.md): reproducible measurement
  definitions without local run receipts.
- [Isolated performance harness](../perf/README.md): synthetic test deployment,
  TLS preflight, worker slot control, and external evidence collection.
- [Short-window observations](../perf/Observations.md): worker, request-lane and
  Docker timings with read-only evidence collection.
- [Third-party notices](../THIRD_PARTY_NOTICES.md): bundled font licenses.

Runtime semantics remain in the external public Runtime Framework and are not
copied into Workspace documentation.
