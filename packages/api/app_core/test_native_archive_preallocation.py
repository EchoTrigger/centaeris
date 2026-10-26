"""A failed never-admitted historical controller stays unchanged during upgrade."""

import copy
import json
from pathlib import Path
from unittest import skipUnless
from unittest.mock import patch

from django.conf import settings
from django.db import connection, transaction
from django.db.migrations.loader import MigrationLoader
from django.test import TransactionTestCase

from . import test_native_archive_bridge as history
from . import test_workspace_snapshot_gc as snapshots
from .models import (
    AgentRun,
    SessionEnvironment,
    AgentRunAuthorization,
    AgentRunEnvironmentRelease,
)
from .native_archive import (
    NativeArchiveBlocked,
    require_native_archive_activation,
    require_native_environment_profile,
)
from .native_archive_preallocation import archived_preallocation_is_unused
from .runtime_contract import (
    _validated_authorization_digest,
    _authorization_signature_for_digest,
)
from .runtime_schema_test_helpers import isolate_runtime_schema


@skipUnless(connection.vendor == "postgresql", "requires PostgreSQL archival ledger")
class NativeArchivePreallocationTests(TransactionTestCase):
    create_owner = snapshots.WorkspaceSnapshotGcTests.create_owner
    store = snapshots.WorkspaceSnapshotGcTests.store

    def setUp(self):
        isolate_runtime_schema(self)
        history.NativeArchivePublicHistoryTests.setUp(self)
        AgentRun.objects.filter(pk=self.run.pk).update(
            status="failed", completedAt=self.old
        )
        self.run.refresh_from_db()
        payload = json.loads(
            (
                Path(__file__).resolve().parents[3]
                / "tests/workspace/fixtures/agent_run_authorization/v1/valid.json"
            ).read_text()
        )
        payload.update(
            id="archived-grant",
            workspaceId=self.workspace.pk,
            userId=str(self.owner.pk),
            agentId=self.session.agent_id,
            sessionId=self.session.pk,
            agentRunId=self.run.pk,
            modelConfigRef=self.model.pk,
            thinkingMode=None,
            artifactScopeRef="artifact_scope_" + self.run.pk,
            assetRefs=[],
            messageAssetRefs=[],
            environmentProfile=self.profile,
            environmentProfileDigest=_validated_authorization_digest(self.profile),
            sessionWorkspace={
                "generation": 0,
                "snapshotSha256": "",
                "snapshotSizeBytes": 0,
                "expandedSizeBytes": 0,
                "fileCount": 0,
            },
        )
        self.payload = payload
        digest = _validated_authorization_digest(payload)
        signature = _authorization_signature_for_digest(
            digest, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY
        )
        old = (
            MigrationLoader(connection)
            .project_state([("app_core", "0031_merge_lifecycle_and_presentation")])
            .apps
        )
        old.get_model("app_core", "AgentRunAuthorization").objects.create(
            id=payload["id"],
            agent_run_id=self.run.pk,
            payload=payload,
            digest=digest,
            signature=signature,
        )
        self.digest = digest
        self.environment_id = self.session.current_environment_id
        SessionEnvironment.objects.filter(pk=self.environment_id).update(
            controller_run=self.run,
            controller_epoch=3,
            controller_lease_owner="historical-cleanup-owner",
            controller_purpose="cleanup",
            controller_released=False,
        )
        self.job_id = "agent_run.lifecycle:" + self.run.pk
        self.failure = {
            "schema": "runtime.agent_run.pre_admission_failure.v1",
            "workspaceId": self.workspace.pk,
            "sessionId": self.session.pk,
            "agentRunId": self.run.pk,
            "authorizationDigest": digest,
            "jobId": self.job_id,
            "leaseOwner": "historical-execute-owner",
            "failedAtMs": 1234,
        }
        with connection.cursor() as cursor:
            cursor.execute("CREATE SCHEMA runtime")
            cursor.execute(
                "CREATE TABLE runtime.schema_migrations(version integer PRIMARY KEY)"
            )
            cursor.executemany(
                "INSERT INTO runtime.schema_migrations VALUES(%s)",
                [(i,) for i in range(1, 25)],
            )
            cursor.execute(
                "CREATE TABLE runtime.runtime_jobs(job_id text PRIMARY KEY,job_kind text,status text,session_id text,idempotency_key text,payload_ref text,lease_owner text,lease_expires_at_ms bigint)"
            )
            cursor.execute(
                "CREATE TABLE runtime.runtime_events(event_id text PRIMARY KEY,session_id text,task_id text,event_type text,visibility text,at_ms bigint,payload_json text)"
            )
            cursor.execute(
                "CREATE TABLE runtime.dead_letters(dead_letter_id text PRIMARY KEY,original_job_id text,status text)"
            )
            cursor.execute(
                "INSERT INTO runtime.runtime_jobs VALUES(%s,'agent_run.lifecycle','dead_lettered',%s,%s,%s,NULL,NULL)",
                [
                    self.job_id,
                    self.session.pk,
                    self.job_id + ":" + digest,
                    "record:agent_run:" + self.run.pk,
                ],
            )
            cursor.execute(
                "INSERT INTO runtime.runtime_events VALUES(%s,%s,%s,%s,'internal',1234,%s)",
                [
                    "agent_run_pre_admission_failure:" + self.run.pk,
                    self.session.pk,
                    self.run.pk,
                    self.failure["schema"],
                    json.dumps(self.failure),
                ],
            )
            cursor.execute(
                "INSERT INTO runtime.dead_letters VALUES('historical-dead-letter',%s,'open')",
                [self.job_id],
            )
            tables = {
                "provider_creation_operations": "environment_id text,agent_run_id text,processing_job_id text",
                "provider_environment_bindings": "environment_id text",
                "provider_run_cleanup_receipts": "environment_id text,agent_run_id text",
                "provider_environment_run_uses": "environment_id text,agent_run_id text",
                "provider_environment_power_operations": "environment_id text",
                "provider_environment_retirements": "environment_id text",
                "provider_environment_provision_completions": "environment_id text",
                "provider_environment_maintenance_completions": "environment_id text",
                "execution_instances": "execution_id text,environment_id text,agent_run_id text",
                "resident_sandboxes": "environment_id text,agent_run_id text",
                "environment_creation_attempts": "allocation_execution_id text",
                "environment_retirements": "environment_id text",
                "agent_run_cleanup_receipts": "environment_id text,agent_run_id text",
                "resource_claims": "session_id text",
            }
            for name, columns in tables.items():
                cursor.execute("CREATE TABLE runtime." + name + "(" + columns + ")")

    def snapshot(self):
        with connection.cursor() as cursor:
            cursor.execute(
                "SELECT row_to_json(e) FROM app_core_sessionenvironment e WHERE id=%s",
                [self.environment_id],
            )
            environment = cursor.fetchone()[0]
            cursor.execute(
                "SELECT row_to_json(j) FROM runtime.runtime_jobs j ORDER BY job_id"
            )
            jobs = cursor.fetchall()
            cursor.execute("SELECT row_to_json(d) FROM runtime.dead_letters d")
            dead_letters = cursor.fetchall()
            cursor.execute("SELECT row_to_json(f) FROM runtime.runtime_events f")
            events = cursor.fetchall()
        return (
            environment,
            jobs,
            dead_letters,
            events,
            AgentRun.objects.values().get(pk=self.run.pk),
            AgentRunAuthorization.objects.values().get(agent_run=self.run),
            AgentRunEnvironmentRelease.objects.count(),
        )

    def test_bound_preallocation_failure_is_archival_only_and_writes_nothing(self):
        before = self.snapshot()
        with transaction.atomic():
            with connection.cursor() as cursor:
                cursor.execute("SET TRANSACTION READ ONLY")
            self.assertTrue(archived_preallocation_is_unused(self.environment_id))
            require_native_archive_activation()
        self.assertEqual(self.snapshot(), before)
        self.assertFalse(
            SessionEnvironment.objects.get(pk=self.environment_id).controller_released
        )
        with self.assertRaises(NativeArchiveBlocked):
            require_native_environment_profile(self.profile)

    def test_archival_eligibility_does_not_allow_another_run_on_old_profile(self):
        with patch("app_core.http.workspaces.request_execution_profile") as dispatch:
            response = self.client.post(
                f"/api/workspaces/{self.workspace.pk}/sessions/{self.session.pk}/messages",
                data=json.dumps(
                    {
                        "operationId": "archived-no-execution",
                        "text": "continue",
                        "modelConfigRef": self.model.pk,
                    }
                ),
                content_type="application/json",
            )
        self.assertEqual(response.status_code, 409, response.content)
        self.assertEqual(
            response.json()["error"],
            "environment_profile_unsupported_create_new_environment",
        )
        dispatch.assert_not_called()

    def test_receipt_identity_and_original_signature_must_match(self):
        for field, value in [
            ("schema", "unknown"),
            ("workspaceId", "other"),
            ("sessionId", "other"),
            ("agentRunId", "other"),
            ("authorizationDigest", "sha256:" + "0" * 64),
            ("jobId", "replay-job"),
            ("leaseOwner", ""),
            ("failedAtMs", 1234.0),
            ("failedAtMs", True),
        ]:
            with self.subTest(field=field), transaction.atomic():
                payload = copy.deepcopy(self.failure)
                payload[field] = value
                with connection.cursor() as cursor:
                    cursor.execute(
                        "UPDATE runtime.runtime_events SET payload_json=%s",
                        [json.dumps(payload)],
                    )
                self.assertFalse(archived_preallocation_is_unused(self.environment_id))
                with self.assertRaises(NativeArchiveBlocked):
                    require_native_archive_activation()
                transaction.set_rollback(True)
        with connection.cursor() as cursor:
            cursor.execute(
                "UPDATE app_core_agentrunauthorization SET signature='hmac-sha256:invalid' WHERE agent_run_id=%s",
                [self.run.pk],
            )
        self.assertFalse(archived_preallocation_is_unused(self.environment_id))

    def test_any_effect_record_or_active_job_prevents_the_exception(self):
        inserts = [
            ("provider_creation_operations", "environment_id", self.environment_id),
            ("provider_environment_bindings", "environment_id", self.environment_id),
            ("provider_environment_run_uses", "environment_id", self.environment_id),
            (
                "provider_environment_power_operations",
                "environment_id",
                self.environment_id,
            ),
            ("provider_environment_retirements", "environment_id", self.environment_id),
            (
                "provider_environment_provision_completions",
                "environment_id",
                self.environment_id,
            ),
            (
                "provider_environment_maintenance_completions",
                "environment_id",
                self.environment_id,
            ),
            ("execution_instances", "environment_id", self.environment_id),
            ("resident_sandboxes", "environment_id", self.environment_id),
            (
                "environment_creation_attempts",
                "allocation_execution_id",
                "orphan-intent",
            ),
            ("environment_retirements", "environment_id", self.environment_id),
            ("agent_run_cleanup_receipts", "environment_id", self.environment_id),
            ("resource_claims", "session_id", self.session.pk),
        ]
        for table, column, value in inserts:
            with self.subTest(table=table), transaction.atomic():
                with connection.cursor() as cursor:
                    cursor.execute(
                        f"INSERT INTO runtime.{table}({column}) VALUES(%s)", [value]
                    )
                self.assertFalse(archived_preallocation_is_unused(self.environment_id))
                with self.assertRaises(NativeArchiveBlocked):
                    require_native_archive_activation()
                transaction.set_rollback(True)
        with connection.cursor() as cursor:
            cursor.execute(
                "UPDATE runtime.runtime_jobs SET status='running',lease_owner='active',lease_expires_at_ms=9999999999999"
            )
        self.assertFalse(archived_preallocation_is_unused(self.environment_id))

    def test_unknown_ledger_schema_or_missing_ledger_fails_closed(self):
        with transaction.atomic():
            with connection.cursor() as cursor:
                cursor.execute("INSERT INTO runtime.schema_migrations VALUES(99)")
            self.assertFalse(archived_preallocation_is_unused(self.environment_id))
            transaction.set_rollback(True)
        with connection.cursor() as cursor:
            cursor.execute(
                "ALTER TABLE runtime.provider_environment_bindings RENAME TO unavailable_bindings"
            )
        with transaction.atomic():
            self.assertFalse(archived_preallocation_is_unused(self.environment_id))
            self.assertEqual(AgentRun.objects.filter(pk=self.run.pk).count(), 1)

    def test_resigned_foreign_original_bindings_or_unknown_schema_are_rejected(self):
        for field, value in [
            ("sessionId", "other-session"),
            ("agentRunId", "other-run"),
            ("workspaceId", "other-workspace"),
            ("userId", "9999"),
            ("environmentProfileDigest", "sha256:" + "0" * 64),
            ("schema", "unknown"),
        ]:
            with self.subTest(field=field), transaction.atomic():
                payload = copy.deepcopy(self.payload)
                payload[field] = value
                digest = _validated_authorization_digest(payload)
                signature = _authorization_signature_for_digest(
                    digest, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY
                )
                with connection.cursor() as cursor:
                    cursor.execute(
                        "UPDATE app_core_agentrunauthorization SET payload=%s,digest=%s,signature=%s WHERE agent_run_id=%s",
                        [json.dumps(payload), digest, signature, self.run.pk],
                    )
                failure = copy.deepcopy(self.failure)
                failure["authorizationDigest"] = digest
                with connection.cursor() as cursor:
                    cursor.execute(
                        "UPDATE runtime.runtime_events SET payload_json=%s",
                        [json.dumps(failure)],
                    )
                    cursor.execute(
                        "UPDATE runtime.runtime_jobs SET idempotency_key=%s",
                        [self.job_id + ":" + digest],
                    )
                self.assertFalse(archived_preallocation_is_unused(self.environment_id))
                transaction.set_rollback(True)

    def test_core_history_or_an_active_noncanonical_replay_job_rejects_archival(self):
        from .models import SessionEvent

        with transaction.atomic():
            SessionEvent.objects.create(
                eventId="contradictory-admission",
                workspace=self.workspace,
                session=self.session,
                agent_run=self.run,
                sequence=1,
                agent_run_sequence=1,
                projects_to_agent_run_stream=True,
                payload={
                    "type": "tool_result",
                    "payload": {"modelContent": "contradictory execution"},
                },
                createdAtMs=1,
            )
            self.assertFalse(archived_preallocation_is_unused(self.environment_id))
            transaction.set_rollback(True)
        with connection.cursor() as cursor:
            cursor.execute(
                "INSERT INTO runtime.runtime_jobs VALUES('noncanonical-replay','agent_run.lifecycle','queued',%s,'replay-key',%s,NULL,NULL)",
                [self.session.pk, "record:agent_run:" + self.run.pk],
            )
        self.assertFalse(archived_preallocation_is_unused(self.environment_id))

    def test_snapshot_reference_or_unpublished_head_prevents_no_effects_claim(self):
        with transaction.atomic():
            # Released publication records remain authoritative even before a
            # confirmation; their identity is seeded through the historical ORM.
            old = (
                MigrationLoader(connection)
                .project_state([("app_core", "0031_merge_lifecycle_and_presentation")])
                .apps
            )
            old.get_model("app_core", "WorkspaceSnapshotPublication").objects.create(
                id="unsettled-snapshot",
                session_id=self.session.pk,
                agent_run_id=self.run.pk,
                authorization_id=self.payload["id"],
                purpose="sessionCommit",
                purpose_key="a" * 64,
                storage_key="retained/snapshot",
                request_digest="sha256:" + "b" * 64,
                generation=1,
                encoding="frameV1",
                snapshot_sha256="sha256:" + "c" * 64,
                snapshot_size_bytes=1,
                expanded_size_bytes=1,
                file_count=1,
            )
            self.assertFalse(archived_preallocation_is_unused(self.environment_id))
            transaction.set_rollback(True)
        snapshots.WorkspaceSnapshotGcTests.snapshot(
            self, session=self.session, run=self.run, current=True
        )
        self.assertFalse(archived_preallocation_is_unused(self.environment_id))

    def test_runtime25_is_readable_without_requiring_a_new_power_table(self):
        with connection.cursor() as cursor:
            cursor.execute("INSERT INTO runtime.schema_migrations VALUES(25)")
        before = self.snapshot()
        with transaction.atomic():
            with connection.cursor() as cursor:
                cursor.execute("SET TRANSACTION READ ONLY")
            self.assertTrue(archived_preallocation_is_unused(self.environment_id))
            require_native_archive_activation()
        self.assertEqual(self.snapshot(), before)

    def test_provider_cleanup_receipt_without_other_ledgers_rejects_archival(self):
        with connection.cursor() as cursor:
            cursor.execute(
                "INSERT INTO runtime.provider_run_cleanup_receipts(environment_id,agent_run_id) VALUES(%s,%s)",
                [self.environment_id, self.run.pk],
            )
        self.assertFalse(archived_preallocation_is_unused(self.environment_id))
        with self.assertRaises(NativeArchiveBlocked):
            require_native_archive_activation()


@skipUnless(connection.vendor == "postgresql", "requires PostgreSQL archival ledger")
class NativeArchiveUnknownProfileTests(TransactionTestCase):
    create_owner = NativeArchivePreallocationTests.create_owner
    store = NativeArchivePreallocationTests.store

    def setUp(self):
        profile = copy.deepcopy(history.ARCHIVED_PROFILE)
        target = profile["templateRef"] if "nested" in self._testMethodName else profile
        target["unknownField"] = "unsupported"
        # Bind the historical profile only once, then sign the grant and all
        # receipt/job anchors through the normal historical fixture producer.
        with patch.object(history, "ARCHIVED_PROFILE", profile):
            NativeArchivePreallocationTests.setUp(self)

    def test_resigned_unknown_profile_field_is_rejected(self):
        self.assertFalse(archived_preallocation_is_unused(self.environment_id))
        with self.assertRaises(NativeArchiveBlocked):
            require_native_archive_activation()

    def test_resigned_nested_unknown_profile_field_is_rejected(self):
        self.assertFalse(archived_preallocation_is_unused(self.environment_id))
        with self.assertRaises(NativeArchiveBlocked):
            require_native_archive_activation()
