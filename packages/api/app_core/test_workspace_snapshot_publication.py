"""Real PostgreSQL and storage boundaries for snapshot publication responsibility."""
from datetime import timedelta
import hashlib
import importlib
import uuid
import json
import os
from pathlib import Path
import tempfile
import threading
from concurrent.futures import ThreadPoolExecutor
from unittest.mock import patch

from django.apps import apps
from django.conf import settings
from django.core.files.storage import default_storage
from django.db import DatabaseError, close_old_connections, connection, transaction
from django.test import Client, TransactionTestCase, override_settings
from django.test.utils import CaptureQueriesContext
from django.utils import timezone

from .http import internal
from .models import AgentRun, Session, StorageCleanupRetry
from .agent_run_authorization_factory import create_agent_run_authorization
from .test_hosted_operations import OperationFixture, PROFILE
from .testing import create_session, leased_lifecycle_job_fixture
from .workspace_snapshot_gc import collect_workspace_snapshot_gc
from .workspace_snapshot_protection import collect_session_workspace_protection


def sha(value):
    return "sha256:" + hashlib.sha256(value).hexdigest()


class WorkspaceSnapshotPublicationTests(OperationFixture, TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        super().setUp()
        directory = tempfile.TemporaryDirectory(prefix="snapshot-publication-")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        storage = override_settings(MEDIA_ROOT=directory.name)
        storage.enable()
        self.addCleanup(storage.disable)
        self.session = create_session(workspace=self.workspace, owner=self.user, agent=self.agent)
        self.run = AgentRun.objects.create(workspace=self.workspace, session=self.session,
            user=self.user, modelConfig=self.model, prompt="snapshot", status="running")
        create_agent_run_authorization(self.run, execution_profile=PROFILE)
        self.owner = leased_lifecycle_job_fixture(self, self.run)

    def wire(self, content=b"saved", *, stage=False, checkpoint="checkpoint-publication", **changes):
        manifest = {"schema":"workspace.snapshot.v1", "files":[{"path":"result.txt",
            "sizeBytes":len(content), "sha256":sha(content), "executable":False}]}
        encoded = json.dumps(manifest, separators=(",", ":")).encode()
        snapshot = len(encoded).to_bytes(4, "big") + encoded + content
        metadata = {"schema":"runtime.execution_workspace.stage.v1" if stage else "runtime.session_workspace.commit.v1",
            "jobId":f"agent_run.lifecycle:{self.run.pk}", "leaseOwner":self.owner,
            "agentRunId":self.run.pk, "authorizationDigest":self.run.authorization.digest,
            "snapshotSha256":sha(snapshot), "snapshotSizeBytes":len(snapshot),
            "expandedSizeBytes":len(content), "fileCount":1}
        if stage:
            metadata["checkpointId"] = checkpoint
        metadata.update(changes)
        header = json.dumps(metadata, separators=(",", ":")).encode()
        return len(header).to_bytes(4, "big") + header + snapshot, metadata, snapshot

    def post(self, wire, *, stage=False, client=None):
        path = "execution-workspace/stage" if stage else "session-workspace/commit"
        return (client or self.client).post("/internal/agent-runs/" + path,
            data=wire, content_type="application/octet-stream",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)

    def records(self):
        return apps.get_model("app_core", "WorkspaceSnapshotPublication").objects.filter(session=self.session)

    def roots(self):
        return [root for root in collect_session_workspace_protection(self.session.pk).roots
                if root.source_kind == "snapshotPublication"]

    def _require_publication_commit_policy(self, *, stage):
        suffix = uuid.uuid4().hex
        audit = connection.ops.quote_name("snapshot_publication_policy_" + suffix)
        function = connection.ops.quote_name("observe_snapshot_policy_" + suffix)
        trigger = connection.ops.quote_name("observe_snapshot_policy_" + suffix)
        with connection.cursor() as cursor:
            cursor.execute("SHOW fsync")
            self.assertEqual(cursor.fetchone()[0], "on")
            cursor.execute(f"CREATE TABLE public.{audit}(publication_id text, state text, policy text)")
            cursor.execute(f"CREATE FUNCTION public.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ "
                f"BEGIN INSERT INTO public.{audit} VALUES(NEW.id,NEW.state,current_setting('synchronous_commit')); "
                "RETURN NEW; END $$")
            cursor.execute(f"CREATE TRIGGER {trigger} AFTER INSERT OR UPDATE ON public.app_core_workspacesnapshotpublication "
                f"FOR EACH ROW EXECUTE FUNCTION public.{function}()")
        def drop_audit():
            with connection.cursor() as cursor:
                cursor.execute("RESET synchronous_commit")
                cursor.execute(f"DROP TRIGGER {trigger} ON public.app_core_workspacesnapshotpublication")
                cursor.execute(f"DROP FUNCTION public.{function}()")
                cursor.execute(f"DROP TABLE public.{audit}")
        self.addCleanup(drop_audit)
        with connection.cursor() as cursor:
            cursor.execute("SET synchronous_commit=off")

        wire, _, _ = self.wire(stage=stage)
        purpose = "executionCheckpoint" if stage else "sessionCommit"
        observed_before_temporary = []
        observer = connection.copy(alias="snapshot-publication-policy-observer")
        original_temporary = internal.tempfile.mkstemp
        def checked_temporary(*args, **kwargs):
            with observer.cursor() as cursor:
                cursor.execute(f"SELECT audit.state,audit.policy,publication.state FROM public.{audit} audit "
                    "JOIN public.app_core_workspacesnapshotpublication publication ON publication.id=audit.publication_id "
                    "WHERE publication.agent_run_id=%s AND publication.purpose=%s", [self.run.pk, purpose])
                observed_before_temporary.append(cursor.fetchone())
            return original_temporary(*args, **kwargs)
        try:
            with patch.object(internal.tempfile, "mkstemp", side_effect=checked_temporary):
                response = self.post(wire, stage=stage)
            with observer.cursor() as cursor:
                cursor.execute(f"SELECT state,policy FROM public.{audit} ORDER BY "
                    "CASE WHEN state='pending' THEN 0 ELSE 1 END")
                policies = cursor.fetchall()
            self.assertEqual(policies, [("pending", "on"), ("staged" if stage else "committed", "on")],
                "both the pending reservation and final publication/head transaction must force durable commit")
            self.assertEqual(observed_before_temporary, [("pending", "on", "pending")],
                "the pending reservation must be committed with synchronous_commit=on before temporary creation")
            self.assertEqual(response.status_code, 201, response.content)
            with connection.cursor() as cursor:
                cursor.execute("SHOW synchronous_commit")
                self.assertEqual(cursor.fetchone()[0], "off",
                    "a publication transaction must not change its caller's session policy")
        finally:
            observer.close()

    def test_session_commit_pending_reservation_forces_durable_commit_before_temporary_creation(self):
        self._require_publication_commit_policy(stage=False)

    def test_execution_stage_pending_reservation_forces_durable_commit_before_temporary_creation(self):
        self._require_publication_commit_policy(stage=True)

    def _require_disabled_fsync_rejection_before_publication_io(self, *, stage):
        wire, _, _ = self.wire(stage=stage)
        def disabled_fsync(execute, sql, params, many, context):
            if sql.strip().upper() == "SHOW FSYNC":
                return execute("SELECT 'off'", None, many, context)
            return execute(sql, params, many, context)
        with connection.execute_wrapper(disabled_fsync), \
                patch.object(internal.tempfile, "mkstemp", wraps=internal.tempfile.mkstemp) as temporary, \
                patch.object(internal, "_store_workspace_snapshot", wraps=internal._store_workspace_snapshot) as stream:
            response = self.post(wire, stage=stage)
        temporary.assert_not_called()
        stream.assert_not_called()
        self.assertEqual(self.records().count(), 0,
            "disabled fsync must reject before reserving a responsibility or copying payload bytes")
        self.assertGreaterEqual(response.status_code, 400, response.content)

    def test_session_commit_disabled_fsync_rejects_before_temporary_creation_or_payload_copy(self):
        self._require_disabled_fsync_rejection_before_publication_io(stage=False)

    def test_execution_stage_disabled_fsync_rejects_before_temporary_creation_or_payload_copy(self):
        self._require_disabled_fsync_rejection_before_publication_io(stage=True)

    def test_pending_responsibility_is_durable_before_temp_and_visible_during_stream(self):
        wire, metadata, _ = self.wire()
        entered, release = threading.Event(), threading.Event()
        responses = []
        original = internal._store_workspace_snapshot
        def blocked(*args, **kwargs):
            entered.set()
            self.assertTrue(release.wait(10))
            return original(*args, **kwargs)
        def upload():
            close_old_connections()
            try:
                responses.append(self.post(wire, client=Client()))
            finally:
                close_old_connections()
        original_temporary = internal.tempfile.mkstemp
        def checked_temporary(*args, **kwargs):
            import psycopg
            config = connection.settings_dict
            with psycopg.connect(dbname=config["NAME"], user=config["USER"], password=config["PASSWORD"],
                                 host=config["HOST"], port=config["PORT"]) as observer:
                row = observer.execute("SELECT state FROM app_core_workspacesnapshotpublication WHERE agent_run_id=%s",
                                       (self.run.pk,)).fetchone()
                self.assertEqual(row, ("pending",), "responsibility must commit before making a temporary name")
            return original_temporary(*args, **kwargs)
        with patch.object(internal, "_store_workspace_snapshot", side_effect=blocked), \
                patch.object(internal.tempfile, "mkstemp", side_effect=checked_temporary):
            thread = threading.Thread(target=upload)
            thread.start()
            try:
                self.assertTrue(entered.wait(10))
                roots = self.roots()
                self.assertEqual(len(roots), 1, "an upload must be retained before streaming bytes")
                record = self.records().get()
                self.assertEqual(record.state, "pending")
                self.assertEqual(record.snapshot_sha256, metadata["snapshotSha256"])
                self.assertEqual(record.authorization_id, self.run.authorization.pk)
                self.assertEqual(record.encoding, "frameV1")
                self.assertEqual(roots[0].storage_key, record.storage_key)
            finally:
                release.set()
                thread.join(10)
        self.assertFalse(thread.is_alive())
        self.assertEqual(responses[0].status_code, 201, responses[0].content)

    def test_stage_without_checkpoint_stays_a_root_and_exact_replay_is_one_record(self):
        wire, _, snapshot = self.wire(stage=True)
        first = self.post(wire, stage=True)
        self.assertEqual(first.status_code, 201, first.content)
        key = first.json()["objectRef"]
        self.assertEqual([root.storage_key for root in self.roots()], [key])
        self.assertEqual(self.records().get().state, "staged")
        with default_storage.open(key, "rb") as source:
            self.assertEqual(source.read(), snapshot)
        self.assertEqual(self.post(wire, stage=True).json(), first.json())
        self.assertEqual(self.records().count(), 1)
        changed, _, _ = self.wire(stage=True, content=b"different")
        conflict = self.post(changed, stage=True)
        self.assertEqual(conflict.status_code, 409, conflict.content)
        self.assertEqual(self.records().count(), 1)

    def test_failed_final_transaction_preserves_pending_and_published_bytes(self):
        wire, _, snapshot = self.wire()
        with patch.object(Session, "save", side_effect=RuntimeError("controlled final rollback")):
            response = self.post(wire)
        self.assertEqual(response.status_code, 500)
        roots = self.roots()
        self.assertEqual(len(roots), 1, "rollback must retain the previously committed write intent")
        record = self.records().get()
        self.assertEqual(record.state, "pending")
        self.session.refresh_from_db()
        self.assertEqual(self.session.workspaceGeneration, 0)
        with default_storage.open(record.storage_key, "rb") as source:
            self.assertEqual(source.read(), snapshot)
        self.assertEqual(self.post(wire).status_code, 201)
        self.assertEqual(self.records().get().state, "committed")

    def test_commit_ack_loss_reloads_durable_committed_state_without_republishing(self):
        wire, _, _ = self.wire()
        original = internal._publish_workspace_snapshot
        def lose_reply(*args, **kwargs):
            original(*args, **kwargs)
            raise RuntimeError("controlled reply loss")
        with patch.object(internal, "_publish_workspace_snapshot", side_effect=lose_reply):
            response = self.post(wire)
        self.assertEqual(response.status_code, 500)
        self.assertEqual(self.records().get().state, "committed")
        record = self.records().get()
        with patch.object(internal, "_link_workspace_snapshot", wraps=internal._link_workspace_snapshot) as link:
            replay = self.post(wire)
        self.assertEqual(replay.status_code, 200, replay.content)
        self.assertEqual(replay.json()["disposition"], "idempotent")
        self.assertEqual(self.records().get().request_digest, record.request_digest)
        self.assertEqual(self.records().count(), 1)

    def test_head_advance_does_not_remove_staged_root_and_purge_blocks_it(self):
        staged, _, _ = self.wire(stage=True)
        staged_response = self.post(staged, stage=True)
        self.assertEqual(staged_response.status_code, 201, staged_response.content)
        key = staged_response.json()["objectRef"]
        committed, _, _ = self.wire(content=b"next")
        self.assertEqual(self.post(committed).status_code, 201)
        self.assertIn(key, [root.storage_key for root in self.roots()])
        self.purge()
        report = collect_workspace_snapshot_gc(timezone.now() + timedelta(seconds=1), False)
        self.assertIn(key, report.blocked)
        self.assertTrue(default_storage.exists(key))

    def test_completed_session_publication_hands_off_and_preserves_explicit_purge_contract(self):
        wire, _, _ = self.wire()
        self.assertEqual(self.post(wire).status_code, 201)
        record = self.records().get()
        self.assertEqual(record.state, "committed")
        self.purge()
        report = collect_workspace_snapshot_gc(timezone.now() + timedelta(seconds=1), False)
        self.assertIn(record.storage_key, report.cleaned)
        self.assertFalse(default_storage.exists(record.storage_key))

    def test_purge_during_stream_never_deletes_then_accepts_a_reference(self):
        wire, _, _ = self.wire()
        original = internal._store_workspace_snapshot
        def purge_after_copy(*args, **kwargs):
            original(*args, **kwargs)
            self.purge()
            report = collect_workspace_snapshot_gc(timezone.now() + timedelta(seconds=1), False)
            self.assertFalse(report.cleaned, "pending publication and its temporary namespace must be fenced")
        with patch.object(internal, "_store_workspace_snapshot", side_effect=purge_after_copy):
            response = self.post(wire)
        self.assertEqual(response.status_code, 409, response.content)
        self.session.refresh_from_db()
        self.assertEqual(self.session.workspaceGeneration, 0)
        self.assertEqual(self.records().get().state, "pending")

    def test_exact_metadata_conflict_and_sql_identity_mutation_are_rejected(self):
        wire, _, _ = self.wire(stage=True)
        self.assertEqual(self.post(wire, stage=True).status_code, 201)
        changed, _, _ = self.wire(stage=True, expandedSizeBytes=999)
        self.assertEqual(self.post(changed, stage=True).status_code, 409)
        record = self.records().get()
        for field, replacement in (("request_digest", "sha256:" + "f" * 64),
                                   ("storage_key", "foreign/key"), ("purpose_key", "f" * 64)):
            with self.subTest(field=field), self.assertRaises(DatabaseError), transaction.atomic():
                self.records().filter(pk=record.pk).update(**{field: replacement})
        self.assertEqual(self.records().get().request_digest, record.request_digest)


    def test_empty_workspace_has_durable_completion_but_no_byte_root(self):
        metadata = {"schema":"runtime.session_workspace.commit.v1", "jobId":f"agent_run.lifecycle:{self.run.pk}",
            "leaseOwner":self.owner, "agentRunId":self.run.pk, "authorizationDigest":self.run.authorization.digest,
            "snapshotSha256":"", "snapshotSizeBytes":0, "expandedSizeBytes":0, "fileCount":0}
        header = json.dumps(metadata).encode()
        response = self.post(len(header).to_bytes(4, "big") + header)
        self.assertEqual(response.status_code, 201, response.content)
        self.assertEqual(self.records().get().state, "committed")
        self.assertEqual(self.roots(), [])

    def test_staged_unknown_duty_ignores_age_and_fake_confirmation_is_rejected(self):
        wire, _, _ = self.wire(stage=True)
        self.assertEqual(self.post(wire, stage=True).status_code, 201)
        record = self.records().get()
        for key in (record.storage_key,):
            os.utime(self.root / key, (1, 1))
        with self.assertRaises(DatabaseError), transaction.atomic():
            self.records().update(state="committed")
        with self.assertRaises(DatabaseError), transaction.atomic():
            self.records().delete()
        self.purge()
        report = collect_workspace_snapshot_gc(timezone.now(), False)
        self.assertIn(record.storage_key, report.blocked)
        self.assertTrue(default_storage.exists(record.storage_key))

    def test_inventory_reads_only_publication_metadata_and_reports_its_own_budget(self):
        for checkpoint in ("first", "second"):
            wire, _, _ = self.wire(stage=True, checkpoint=checkpoint)
            self.assertEqual(self.post(wire, stage=True).status_code, 201)
        with CaptureQueriesContext(connection) as captured:
            inventory = collect_session_workspace_protection(self.session.pk, max_records=1)
        self.assertIn("snapshotPublicationsTruncated", inventory.unresolved)
        self.assertEqual(sum(root.source_kind == "snapshotPublication" for root in inventory.roots), 1)
        publication_query = [item["sql"] for item in captured if "app_core_workspacesnapshotpublication" in item["sql"]]
        self.assertEqual(len(publication_query), 1)
        self.assertNotIn('"signature"', publication_query[0])
        self.assertNotIn('"payload"', publication_query[0])
        self.assertNotIn('"agent_instructions"', publication_query[0])
        self.assertTrue(any(reason.startswith("snapshotPublicationUnconfirmed:") for reason in inventory.unresolved))

    def test_wrong_authorization_fk_and_confirmation_without_head_cannot_be_inserted(self):
        wire, _, _ = self.wire()
        original = internal._store_workspace_snapshot
        def inspect(*args, **kwargs):
            record = self.records().get()
            with self.assertRaises(DatabaseError), transaction.atomic():
                self.records().filter(pk=record.pk).update(state="committed", confirmed_at=timezone.now())
            return original(*args, **kwargs)
        with patch.object(internal, "_store_workspace_snapshot", side_effect=inspect):
            self.assertEqual(self.post(wire).status_code, 201)
        other = create_session(workspace=self.workspace, owner=self.user, agent=self.agent)
        other_run = AgentRun.objects.create(workspace=self.workspace, session=other, user=self.user,
            modelConfig=self.model, prompt="other", status="completed")
        grant = create_agent_run_authorization(other_run, execution_profile=PROFILE)
        record = self.records().get()
        values = {field.attname: getattr(record, field.attname) for field in record._meta.concrete_fields}
        values.update(id="forged", state="pending", confirmed_at=None, authorization_id=grant.pk)
        with self.assertRaises(DatabaseError), transaction.atomic():
            self.records().model.objects.create(**values)


    def test_gc_and_publisher_share_owner_fence_while_pending_stream_is_unlocked(self):
        wire, _, _ = self.wire()
        entered, release = threading.Event(), threading.Event()
        original = internal._store_workspace_snapshot
        def blocked(*args, **kwargs):
            entered.set()
            self.assertTrue(release.wait(10))
            return original(*args, **kwargs)
        def upload():
            close_old_connections()
            try:
                return self.post(wire, client=Client())
            finally:
                close_old_connections()
        def collect():
            close_old_connections()
            try:
                return collect_workspace_snapshot_gc(timezone.now() + timedelta(seconds=1), False)
            finally:
                close_old_connections()
        with ThreadPoolExecutor(max_workers=2) as pool, \
                patch.object(internal, "_store_workspace_snapshot", side_effect=blocked):
            uploading = pool.submit(upload)
            try:
                self.assertTrue(entered.wait(10))
                self.purge()
                with transaction.atomic():
                    self.agent.__class__.objects.select_for_update().get(pk=self.agent.pk)
                    Session.objects.select_for_update().get(pk=self.session.pk)
                    collecting = pool.submit(collect)
                    with self.assertRaises(TimeoutError):
                        collecting.result(timeout=0.1)
                report = collecting.result(timeout=10)
                self.assertFalse(report.cleaned)
                self.assertTrue(any(key.endswith(".tmp") for key in report.blocked))
            finally:
                release.set()
            response = uploading.result(timeout=10)
        self.assertEqual(response.status_code, 409, response.content)
        self.assertEqual(self.records().get().state, "pending")
        self.session.refresh_from_db()
        self.assertEqual(self.session.workspaceGeneration, 0)

    def test_forward_migration_preserves_authoritative_records_without_inventing_old_publications(self):
        from django.db.migrations.loader import MigrationLoader
        loader = MigrationLoader(connection)
        before_state = loader.project_state([("app_core", "0026_external_identity_login")])
        after_state = loader.project_state([("app_core", "0027_workspace_snapshot_publication")])
        migration = importlib.import_module("app_core.migrations.0027_workspace_snapshot_publication").Migration("0027_workspace_snapshot_publication", "app_core")
        before_session = Session.objects.values().get(pk=self.session.pk)
        before_grant = self.run.authorization.__class__.objects.values().get(pk=self.run.authorization.pk)
        namespace = "snapshot_publication_migration_" + uuid.uuid4().hex
        quoted = connection.ops.quote_name(namespace)
        with connection.cursor() as cursor:
            cursor.execute("CREATE SCHEMA " + quoted)
            cursor.execute("SHOW search_path")
            original_path = cursor.fetchone()[0]
            cursor.execute("SET search_path TO " + quoted + ",public")
        try:
            # Apply the actual new migration in an empty owned schema. Existing
            # authoritative Session/Run/grant rows remain in public and are not copied.
            with connection.schema_editor() as editor:
                for operation in migration.operations:
                    operation.database_forwards("app_core", editor, before_state, after_state)
            self.assertEqual(self.records().count(), 0)
            self.assertEqual(Session.objects.values().get(pk=self.session.pk), before_session)
            self.assertEqual(self.run.authorization.__class__.objects.values().get(pk=self.run.authorization.pk), before_grant)
            inventory = collect_session_workspace_protection(self.session.pk)
            self.assertIn("inflightWorkspaceUploadsNotIndexed", inventory.unresolved)
        finally:
            with connection.cursor() as cursor:
                cursor.execute("SELECT set_config('search_path',%s,false)", [original_path])
                cursor.execute("DROP SCHEMA " + quoted + " CASCADE")


    def test_advanced_head_uses_real_references_instead_of_permanent_committed_publication_pin(self):
        first, _, _ = self.wire(content=b"first head")
        self.assertEqual(self.post(first).status_code, 201)
        prior = self.records().get()
        self.session.refresh_from_db()
        old_key = self.session.workspaceStorageKey
        self.assertEqual(prior.state, "committed")
        self.assertEqual(self.roots(), [], "completed handoff is not an independent permanent pin")
        AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
        self.run = AgentRun.objects.create(workspace=self.workspace, session=self.session,
            user=self.user, modelConfig=self.model, prompt="successor", status="running")
        create_agent_run_authorization(self.run, execution_profile=PROFILE)
        self.owner = leased_lifecycle_job_fixture(self, self.run)
        second, _, _ = self.wire(content=b"second head")
        self.assertEqual(self.post(second).status_code, 201)
        self.session.refresh_from_db()
        self.assertEqual(self.session.workspaceGeneration, 2)
        inventory = collect_session_workspace_protection(self.session.pk)
        self.assertFalse(any(root.source_kind == "snapshotPublication" for root in inventory.roots))
        self.assertTrue(any(root.source_kind == "sessionHead" and root.storage_key == self.session.workspaceStorageKey
                            for root in inventory.roots))
        self.assertTrue(any(root.source_kind == "runAuthorization" and root.storage_key == old_key
                            for root in inventory.roots), "the successor's frozen input still retains the prior head")
        self.assertEqual(self.records().count(), 2, "handoff evidence remains immutable history")


    def _blocked_publication_retry_budget(self, *, stage):
        wire, _, _ = self.wire(stage=stage)
        if stage:
            self.assertEqual(self.post(wire, stage=True).status_code, 201)
        else:
            with patch.object(Session, "save", side_effect=RuntimeError("controlled pending final commit")):
                self.assertEqual(self.post(wire).status_code, 500)
        record = self.records().get()
        self.purge()
        start = timezone.now() + timedelta(days=1)
        with patch.object(default_storage, "delete", side_effect=AssertionError("legal wait must not unlink")):
            for step in range(8):
                instant = start + timedelta(minutes=6 * step)
                with patch("app_core.storage_gc_retry.timezone.now", return_value=instant):
                    report = collect_workspace_snapshot_gc(instant, False)
                self.assertIn(record.storage_key, report.blocked)
                self.assertTrue(default_storage.exists(record.storage_key))
        self.assertEqual(StorageCleanupRetry.objects.filter(storageKey=record.storage_key).count(), 0,
            "unresolved publication is a legitimate wait, not a failed deletion attempt")

    def test_pending_publication_wait_does_not_create_or_exhaust_gc_retry_budget(self):
        self._blocked_publication_retry_budget(stage=False)

    def test_staged_publication_wait_does_not_create_or_exhaust_gc_retry_budget(self):
        self._blocked_publication_retry_budget(stage=True)

    def purge(self):
        old = timezone.now() - timedelta(days=2)
        AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
        Session.objects.filter(pk=self.session.pk).update(status="deleted", deletedAt=old, purgedAt=old)
