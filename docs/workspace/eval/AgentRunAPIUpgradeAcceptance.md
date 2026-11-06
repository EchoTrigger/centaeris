# Agent Run API and workspace resource upgrade acceptance

This checklist covers the `0045_one_shot_cleanup` →
`0046_workspace_owned_resources` upgrade in the patch based on
`aaacd11498b4c85f09faa79118516d69adc93f5b`. It records acceptance obligations
separately from the evidence obtained during review. **The real PostgreSQL
upgrade has not been executed in the review environment.** The Agent Run API v1
HTTP, idempotency, delivery and cancellation contracts remain unchanged.

Read this alongside [workspace resource administration](../operations/WorkspaceResourceAdministration.md),
the [test policy](../../development/Testing.md#test-quality) and the
[workspace release gate](ReleaseGate.md). The implementation under test is
[migration 0046](../../../packages/api/app_core/migrations/0046_workspace_owned_resources.py).

## Evidence obtained and outstanding work

| Check | Recorded result | What the result establishes |
| --- | --- | --- |
| Empty Agent Run API plugin scope, invalid credential binding, nonempty plugin filtering, interactive plugin loading | Four `AgentRunApiPluginScopeTests` passed. Before the fix, the empty-scope and invalid-binding cases errored on an unrelated missing plugin. | Empty scope does not load the optional catalog; existing nonempty and interactive behavior is preserved. No database or HTTP admission was exercised. |
| Refusal to reverse secret retirement | `WorkspaceResourceRetirementMigrationTests` passed. It calls the real `Migration.unapply(ProjectState(), schema_editor_stub)` and requires `IrreversibleError`. Before the fix, reversal returned without that error. | Django rejects reverse migration before database access. It does not prove a real upgrade or backup restore. |
| Earlier focused Agent Run API PostgreSQL attempt | 14 tests discovered, **0 executed**; setup failed because the PostgreSQL connection on port `55432` was refused. | Blocked. Discovery and a setup failure are not passing tests. |
| Historical-state migration integration test supplied with this patch | Source parsed/imported; 1 test discovered, **0 executed**. The 0045/0046 historical migration graphs loaded without a database. | Discovery and source validation succeeded. The actual PostgreSQL upgrade assertions remain pending. |
| Synthetic plugin fixture and acceptance document | Catalog validation, legacy/content-addressed path resolution and document link checks completed without a database. | The fixture and links are usable; these checks do not exercise migration persistence, live plugin execution or restore. |
| Authenticated history delivery, live model/connector boundaries after retirement, cleanup, and backup restoration | Pending in a disposable deployment with real PostgreSQL and the required services. | These remain release acceptance requirements. |

The five executed no-database regressions are in
[Agent Run API authority tests](../../../packages/api/app_core/test_agent_run_api_authority.py)
and [workspace resource tests](../../../packages/api/app_core/test_workspace_owned_resources.py).
The historical-state test is in
[workspace resource migration tests](../../../packages/api/app_core/test_workspace_owned_resources_migration.py).
Do not replace these statuses with “passed” from a test-discovery count, skipped
test, mocked database, SQLite run or direct invocation of the migration function
against the current application registry.

Candidate integration ran `WorkspaceOwnedResourceUpgradeTests` on disposable
PostgreSQL 18.6. One test executed and passed. It covered the supplied
assertions for PG-01 through PG-06 and PG-09. The additional HTTP, live-service,
negative migration and restoration checks in the matrix still need deployment evidence.

## Fixture and execution prerequisites

Use a disposable PostgreSQL instance with the version and extensions required by
the repository's CI/deployment configuration. Configure `TEST_POSTGRES_DB`,
`TEST_POSTGRES_USER`, `TEST_POSTGRES_PASSWORD`, `TEST_POSTGRES_HOST` and
`TEST_POSTGRES_PORT` through the normal test environment. The fixture account
must be able to create and drop its isolated test databases. Keep passwords and
credential values out of command transcripts and reports.

The integration test uses
[`isolated_migration_database`](../../../packages/api/app_core/migration_testing.py).
It leaves the runner's shared schema intact, creates a separate database at
0045, obtains historical models from `MigrationExecutor.loader.project_state`,
seeds those historical models and migrates that database to 0046. It also routes
the current read services used by the test to that same isolated database. It
asserts PostgreSQL as the backend; SQLite is not a substitute for this gate.

The supplied fixture contains two workspaces with different owners, a shared
provider/model/key, local providers/models/keys, a shared MCP key and a key owned
only by workspace A. Both workspaces enable the same plugin name. A has only a
legacy enablement; B already pins a different version in a workspace
installation. The fixture includes legacy package files, content-addressed
package files, Agent Run API credentials bound to shared/local models, an already
revoked credential and a completed interactive run with an event and historical
signed authorization.

The smallest executable checks from the repository root are:

```sh
DJANGO_SETTINGS_MODULE=api.test_settings .venv/bin/python packages/api/manage.py test \
  app_core.test_agent_run_api_authority.AgentRunApiPluginScopeTests \
  app_core.test_workspace_owned_resources.WorkspaceResourceRetirementMigrationTests \
  --verbosity 2 --noinput
```

That command does not need PostgreSQL. Once the database prerequisite is met,
run the actual historical upgrade test:

```sh
DJANGO_SETTINGS_MODULE=api.test_settings .venv/bin/python packages/api/manage.py test \
  app_core.test_workspace_owned_resources_migration.WorkspaceOwnedResourceUpgradeTests \
  --verbosity 2 --noinput
```

The directly related database regressions can then be run without selecting the
entire API suite:

```sh
DJANGO_SETTINGS_MODULE=api.test_settings .venv/bin/python packages/api/manage.py test \
  app_core.test_workspace_owned_resources.WorkspaceOwnedResourceTests \
  app_core.test_workspace_resource_admin \
  app_core.test_agent_run_api_authority.AgentRunApiCredentialAuthorityTests \
  app_core.test_agent_run_api \
  app_core.test_agent_run_api_cleanup \
  --verbosity 2 --noinput
```

Retain the exact discovered and executed test identities, counts, command, exit
code and output. The broader CI/release gate remains a separate requirement;
these focused commands do not claim to replace it.

## Real PostgreSQL acceptance cases

“Supplied test” identifies the assertions exercised by the passing historical-state
integration test. Additional evidence in the same row remains outstanding.

| ID | Exercise and expected result | Supplied test coverage / additional evidence required |
| --- | --- | --- |
| PG-01: historical state | Build an isolated database at 0045, seed historical models and apply 0046 with `MigrationExecutor`. Confirm the migration recorder advances to 0046. Do not recreate or downgrade the runner's shared database. | Supplied test. Record the real database vendor/version, command, discovered/executed identities and before/after migration state. |
| PG-02: shared model retirement | Shared provider/model identities remain present for historical foreign keys. Current shared models become disabled and noncurrent; shared providers become disabled and archived. Their provider credentials and all global MCP credentials are deleted. A provider-less diagnostic model is a separate existing exception, not a workspace secret. | Supplied test covers retirement, retained model identity, removed secrets and current model selection. Add pre/post safe inventories for multiple revisions and already archived providers in a representative restored fixture. |
| PG-03: local resources and Agent Run API credentials | Workspace provider/model records, credential ciphertext, versions and owners remain unchanged. An unrevoked Agent Run API credential bound to a retired model becomes revoked. An already revoked credential keeps its original timestamp. A local-model credential is not revoked by this migration. | Supplied test covers local resource rows and the three revocation cases. Verify live authentication separately; retaining a row does not itself grant execution permission. |
| PG-04: plugin bindings and bytes | Each legacy enablement without an installation becomes an installation containing the matching old catalog snapshot. An existing installation retains its package, owner and timestamp, including a different version of the same plugin name. Resolve the migrated legacy installation to the original legacy files and the existing pinned installation to its original content-addressed files. | Supplied test covers both paths, installed metadata and unchanged file bytes. For deployment acceptance, retain package/content hashes and demonstrate that the required legacy volume is mounted in every consuming service. The migration does not copy legacy bytes into content-addressed storage. |
| PG-05: historical reads | Completed interactive run IDs, model/provider references, prompt/events and signed authorization payload/digest/signature remain unchanged. A current authorized owner can read that history after platform secrets are retired; a foreign owner cannot. Reading history must not invoke the old model or connector. | Supplied test compares persisted rows, exercises current transcript authorization and serializes the retired model identity. Actual authenticated HTTP transcript/event/artifact delivery from the upgraded deployment is still required; the test does not start Runtime or storage services. |
| PG-06: workspace isolation | Model selection only exposes the requesting workspace's configured models. The same plugin name or credential reference in another workspace must not supply a fallback key. Foreign IDs must fail through management and execution routes without exposing secret values. | Supplied test covers model selection and the no-fallback MCP lookup when B lacks a key and A has one. Run the existing management/authority regressions and exercise model/connector gateways on the upgraded fixture; a query helper alone does not prove every HTTP boundary. |
| PG-07: unsettled work and revocation | On a disposable live fixture, retire/revoke authority after admission. Further output and new model/connector calls must respect current authority checks. Reconciliation requests cancellation of active work, and cleanup retains only the restricted original signed authority. Do not infer that an already issued external side effect was rolled back. | Pending live service acceptance. Record run/operation IDs, observed gateway rejections, authoritative terminal state, environment release and cleanup receipts. Public GET/cancel still reject a revoked Agent Run API credential under the unchanged v1 contract. |
| PG-08: missing or mismatched plugin storage | On separate 0045 fixtures, remove the catalog, remove a required legacy package directory, or create an enablement absent from the catalog. Upgrade must fail without recording 0046 or partially retiring configuration. Restore the matching original data and retry. A different package with the same name is not a valid repair. | Pending negative migration/deployment acceptance. Capture the error, unchanged migration recorder and pre/post row inventories. Validate actual package content against its recorded digest before deployment; the Python catalog loader's metadata/directory checks do not establish full package-byte integrity. |
| PG-09: irreversible rollback | After a successful forward upgrade, request migration back to 0045. Require `IrreversibleError`, 0046 still recorded and retired secrets still absent. An application-code rollback must not be represented as restored configuration. | Supplied test invokes real executor reversal and checks recorder/secret state. The separately executed no-database guard is supporting evidence only. |
| PG-10: backup restoration | Restore the pre-upgrade database and matching storage into a second disposable deployment using the corresponding old release/configuration. Verify plugin resolution, authorized history reads and synthetic credential usability. Restore credential-encryption configuration securely. | Pending restoration drill. Retain backup/restore job identities, matching storage hashes, source/image identities and observed results. The forward migration test cannot prove backups are complete or decryptable. |

## Preflight and recovery boundaries

Before planned retirement, settle in-flight work and close its delivery/cleanup
liabilities using the old service versions. Identify which workspaces still
depend on shared model or MCP keys, and provision explicitly owned replacement
credentials. Membership, model availability and the issuing credential remain
part of current Agent Run API authority. A replacement credential does not inherit
another credential's operation IDs. Emergency revocation remains available;
its impact on pending work must be recorded rather than hidden by key rotation.

Inventory every `(workspace, pluginName)` enablement without an installation.
For those entries, preserve the exact legacy catalog and package directories
from `PLUGIN_CATALOG_ROOT`; also preserve the content-addressed packages already
referenced by installations or historical grants. Confirm package identities
and bytes before moving or replacing storage. A metadata-only database backup
does not preserve these files. Confirm required transcript/artifact and other
durable service storage as part of the corresponding deployment backup.

Migration 0046 deliberately deletes shared secrets and declares no reverse
function. There is no valid automatic inverse that can reconstruct those
secrets. Recovery uses a coordinated pre-upgrade database/storage/configuration
restore, followed by verification in isolation. Restoring the database does not
undo external calls that already occurred. Keep execution stopped during the
restore drill and reconcile outstanding operation identities before any resumed
work; do not produce duplicate actions by treating uncertain prior work as a
new operation.

## Evidence required for an acceptance decision

The final report should include the following without plaintext secrets or
customer payloads. Use synthetic inputs for executable acceptance fixtures.

| Evidence | Required contents |
| --- | --- |
| Source | Baseline commit, candidate commit/tree, complete patch SHA-256 when the tree is uncommitted, and tested service/image identities. |
| Test execution | Exact command and test IDs; discovered, executed, passed, failed and skipped counts; exit code; setup failures distinguished from executed test failures. |
| Database | PostgreSQL version, disposable fixture identity, historical target, forward target and migration recorder before/after forward and refused reverse attempts. |
| Preserved facts | Workspace/resource IDs and scope, credential versions and ciphertext fingerprints, historical row/payload hashes, and plugin snapshot/content hashes. |
| Read and isolation behavior | Authorized and foreign request results, absence of secret values, selected model/plugin identities, and gateway rejection observations. |
| Lifecycle | Original operation/run IDs, revocation/cancellation observations, terminal facts and physical release/cleanup evidence. |
| Recovery | Pre-upgrade backup identities, matching storage/configuration versions, isolated restore output and successful read/credential verification. |
| Remaining gaps | Each unexecuted case, missing prerequisite and owner of the next verification step. “0 executed” remains blocked even if discovery succeeded. |

Acceptance requires passing evidence for the relevant PostgreSQL cases and the
deployment/recovery cases above. The five local regressions, source review and
generated patch establish a reviewable candidate; they do not certify an
upgrade deployment.
