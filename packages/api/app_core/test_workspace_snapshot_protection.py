"""Read-only inventory of existing snapshot roots; never a deletion decision."""
import copy
import hashlib
import io
import json
from types import SimpleNamespace

from unittest.mock import patch
from django.core.files.storage import default_storage
from django.core.management import call_command
from django.core.management.base import CommandError
from django.db import DatabaseError, connection
from django.test import SimpleTestCase, TestCase
from django.test.utils import CaptureQueriesContext

from .runtime_contract import session_workspace_for_session
from .core_test_fixtures import core_fixture
from .workspace_snapshot_protection import _read_checkpoint, collect_session_workspace_protection, snapshot_protection_for_session
from .models import AgentRun, Session
from .testing import create_session
from .test_hosted_operations import OperationFixture, PROFILE
from .agent_run_authorization_factory import create_agent_run_authorization


def descriptor(generation=1, content=b"workspace"):
    return {"generation": generation, "snapshotSha256": "sha256:" + hashlib.sha256(content).hexdigest(),
            "snapshotSizeBytes": len(content), "expandedSizeBytes": len(content), "fileCount": 1}


def snapshot_key(value, session_id="session"):
    return f"workspaces/workspace/sessions/{session_id}/snapshots/{value['generation']}/{value['snapshotSha256'][7:]}.snapshot"


def session(value=None):
    value = descriptor(2) if value is None else value
    return SimpleNamespace(id="session", workspace_id="workspace", workspaceGeneration=value["generation"],
        workspaceStorageKey=snapshot_key(value) if value["snapshotSizeBytes"] else "",
        workspaceSnapshotSha256=value["snapshotSha256"], workspaceSnapshotSizeBytes=value["snapshotSizeBytes"],
        workspaceExpandedSizeBytes=value["expandedSizeBytes"], workspaceFileCount=value["fileCount"])


def authorization(value=None, *, run_id="run", status="queued"):
    return SimpleNamespace(id="authorization-" + run_id, digest="sha256:" + "a" * 64,
        agent_run=SimpleNamespace(id=run_id, session_id="session", workspace_id="workspace", status=status),
        payload={"workspaceId": "workspace", "sessionId": "session", "agentRunId": run_id,
                 "sessionWorkspace": descriptor() if value is None else value})


def complete_checkpoint_payload(checkpoint_id, session_id, run_id, digest, snapshot):
    # Canonical serialization of Core RuntimeRecoveryCheckpointV1, including
    # ExecutionWorkspaceGeneration rather than a partial API reference shape.
    return {"schema": "runtime.recovery_checkpoint.v1", "checkpointId": checkpoint_id,
        "sessionId": session_id, "agentRunId": run_id, "executionId": "execution-fixture",
        "authorizationDigest": digest, "sessionSequence": 1, "modelRequestId": "model-request-fixture",
        "workspaceSnapshot": snapshot,
        "workspaceGeneration": {"status": "known", "token": {"instanceEpoch": "fixture-host", "generation": 0}},
        "createdAtMs": 1}

def checkpoint(auth, *, checkpoint_id="recovery", snapshot=None):
    value = descriptor() if snapshot is None else snapshot
    key = (f"workspaces/workspace/sessions/session/agent-runs/{auth.agent_run.id}/execution-checkpoints/"
           f"{hashlib.sha256(checkpoint_id.encode()).hexdigest()}/{value['snapshotSha256'][7:]}.snapshot")
    payload = complete_checkpoint_payload(checkpoint_id, "session", auth.agent_run.id, auth.digest,
        {**{name: value[name] for name in value if name != "generation"}, "objectRef": key})
    return checkpoint_id, json.dumps(payload), key


class SnapshotProtectionRecordTests(SimpleTestCase):
    def inventory(self, *, owner=None, grants=(), checkpoints=()):
        return snapshot_protection_for_session(session() if owner is None else owner,
            authorizations=grants, recovery_checkpoints=checkpoints)

    def test_advanced_head_keeps_frozen_old_version_even_after_run_terminal(self):
        for status in ("queued", "running", "completed", "failed", "cancelled"):
            with self.subTest(status=status):
                grant = authorization(status=status)
                result = self.inventory(grants=[grant])
                self.assertEqual({root.storage_key for root in result.roots},
                    {snapshot_key(descriptor()), snapshot_key(descriptor(2))})
                old = next(root for root in result.roots if root.source_kind == "runAuthorization")
                self.assertEqual((old.source_id, old.agent_run_id), (grant.id, "run"))

    def test_retains_each_root_when_head_and_authorization_share_content(self):
        grant = authorization(descriptor(2))
        result = self.inventory(grants=[grant])
        self.assertEqual(len(result.roots), 2)
        self.assertEqual({root.storage_key for root in result.roots}, {snapshot_key(descriptor(2))})

    def test_recovery_checkpoint_keeps_its_exact_separate_object(self):
        grant = authorization()
        checkpoint_id, payload, key = checkpoint(grant)
        result = self.inventory(grants=[grant], checkpoints=[(checkpoint_id, payload)])
        root = next(root for root in result.roots if root.source_kind == "recoveryCheckpoint")
        self.assertEqual((root.storage_key, root.source_id, root.agent_run_id), (key, checkpoint_id, "run"))

    def test_session_workspace_checkpoint_retains_the_frozen_grant_object_after_head_advances(self):
        grant = authorization(descriptor(3))
        checkpoint_id, serialized, _key = checkpoint(grant)
        payload = json.loads(serialized)
        payload["workspaceSnapshot"]["objectRef"] = "session-workspace:generation:3"
        result = self.inventory(grants=[grant], checkpoints=[(checkpoint_id, json.dumps(payload))])
        root = next(root for root in result.roots if root.source_kind == "recoveryCheckpoint")
        self.assertEqual((root.storage_key, root.source_id), (snapshot_key(descriptor(3)), checkpoint_id))
        self.assertFalse(any("Invalid" in gap for gap in result.unresolved))

    def test_session_workspace_checkpoint_rejects_generation_and_each_descriptor_mismatch(self):
        grant = authorization(descriptor(3))
        checkpoint_id, serialized, _key = checkpoint(grant)
        payload = json.loads(serialized)
        payload["workspaceSnapshot"]["objectRef"] = "session-workspace:generation:3"
        for change in ({"objectRef": "session-workspace:generation:4"},
                       {"objectRef": "session-workspace:generation:03"},
                       {"snapshotSha256": "sha256:" + "b" * 64}, {"snapshotSizeBytes": 10},
                       {"expandedSizeBytes": 10}, {"fileCount": 2}, {"unexpected": True}):
            changed = copy.deepcopy(payload)
            changed["workspaceSnapshot"].update(change)
            with self.subTest(change=change):
                result = self.inventory(grants=[grant], checkpoints=[(checkpoint_id, json.dumps(changed))])
                self.assertFalse(any(root.source_kind == "recoveryCheckpoint" for root in result.roots))
                self.assertIn("recoveryCheckpointInvalid:" + checkpoint_id, result.unresolved)
    def assert_checkpoint_unresolved(self, payload, *, serialized=None):
        grant = authorization()
        result = self.inventory(grants=[grant], checkpoints=[(payload["checkpointId"],
            json.dumps(payload) if serialized is None else serialized)])
        self.assertFalse(any(root.source_kind == "recoveryCheckpoint" for root in result.roots))
        self.assertIn("recoveryCheckpointInvalid:" + payload["checkpointId"], result.unresolved)
        self.assertTrue(any(root.source_kind == "runAuthorization" for root in result.roots))

    def test_checkpoint_closed_schema_rejects_each_missing_and_unknown_field(self):
        _checkpoint_id, serialized, _key = checkpoint(authorization())
        original = json.loads(serialized)
        for session_ref in (False, True):
            payload = copy.deepcopy(original)
            if session_ref:
                payload["workspaceSnapshot"]["objectRef"] = "session-workspace:generation:1"
            for field in payload:
                changed = {key: value for key, value in payload.items() if key != field}
                with self.subTest(session_ref=session_ref, missing=field):
                    # Keep the external record ID even when the payload omits it.
                    result = self.inventory(grants=[authorization()], checkpoints=[("recovery", json.dumps(changed))])
                    self.assertFalse(any(root.source_kind == "recoveryCheckpoint" for root in result.roots))
                    self.assertIn("recoveryCheckpointInvalid:recovery", result.unresolved)
            with self.subTest(session_ref=session_ref, unknown=True):
                self.assert_checkpoint_unresolved(payload | {"unexpected": True})

    def test_checkpoint_identity_and_integer_boundaries_are_validated(self):
        _checkpoint_id, serialized, _key = checkpoint(authorization())
        payload = json.loads(serialized)
        changes = [{field: value} for field in ("executionId", "modelRequestId")
                   for value in ("", " ", " outer", "outer ", None, 1)]
        changes += [{"sessionSequence": value} for value in (0, -1, True, 1.0, 2**64)]
        changes += [{"createdAtMs": value} for value in (-1, True, 1.0, 2**63)]
        for change in changes:
            with self.subTest(change=change):
                self.assert_checkpoint_unresolved(payload | change)

    def test_workspace_generation_tag_fields_and_rust_ranges_are_closed(self):
        _checkpoint_id, serialized, _key = checkpoint(authorization())
        payload = json.loads(serialized)
        invalid = [None, {}, {"status": "unsupported"}, {"status": "known"},
            {"status": "known", "token": {"instanceEpoch": "host", "generation": 0}, "extra": True},
            {"status": "unknown"}, {"status": "unknown", "reason": "", "extra": True}]
        invalid += [{"status": "known", "token": token} for token in (
            {}, {"generation": 0}, {"instanceEpoch": "host"},
            {"instanceEpoch": "host", "generation": 0, "extra": True},
            *({"instanceEpoch": epoch, "generation": 0} for epoch in ("", "é" * 81, "host\x00epoch", None)),
            *({"instanceEpoch": "host", "generation": value} for value in (-1, True, 1.0, 2**64)),
        )]
        invalid += [{"status": "unknown", "reason": value} for value in ("", " \n", None, 0)]
        invalid.append({"status": "unknown", "reason": "not observed", "token": {"instanceEpoch": "host", "generation": 0}})
        for value in invalid:
            with self.subTest(generation=value):
                self.assert_checkpoint_unresolved(payload | {"workspaceGeneration": value})

    def test_snapshot_closed_schema_and_unsigned_integer_ranges_are_validated(self):
        _checkpoint_id, serialized, _key = checkpoint(authorization())
        payload = json.loads(serialized)
        original = payload["workspaceSnapshot"]
        changed_snapshots = [{key: value for key, value in original.items() if key != field} for field in original]
        changed_snapshots.append(original | {"unexpected": True})
        for field, maximum in (("snapshotSizeBytes", 2**64), ("expandedSizeBytes", 2**64), ("fileCount", 2**32)):
            changed_snapshots.extend(original | {field: value} for value in (-1, True, 1.0, maximum))
        for snapshot in changed_snapshots:
            with self.subTest(snapshot=snapshot):
                self.assert_checkpoint_unresolved(payload | {"workspaceSnapshot": snapshot})

    def test_empty_checkpoint_accepts_null_or_omitted_ref_and_rejects_blank(self):
        _checkpoint_id, serialized, _key = checkpoint(authorization())
        payload = json.loads(serialized)
        empty = {"objectRef": None, "snapshotSha256": "", "snapshotSizeBytes": 0,
                 "expandedSizeBytes": 0, "fileCount": 0}
        result = self.inventory(grants=[authorization()], checkpoints=[("recovery", json.dumps(payload | {"workspaceSnapshot": empty}))])
        self.assertFalse(any("Invalid" in gap for gap in result.unresolved))
        omitted = {name: value for name, value in empty.items() if name != "objectRef"}
        result = self.inventory(grants=[authorization()], checkpoints=[("recovery", json.dumps(payload | {"workspaceSnapshot": omitted}))])
        self.assertFalse(any("Invalid" in gap for gap in result.unresolved))
        for change in ({"objectRef": ""}, {"objectRef": " "}, {"expandedSizeBytes": 1},
                       {"fileCount": 1}, {"snapshotSizeBytes": 1}, {"snapshotSha256": "sha256:" + "a" * 64}):
            with self.subTest(change=change):
                self.assert_checkpoint_unresolved(payload | {"workspaceSnapshot": empty | change})

    def test_checkpoint_rejects_duplicate_json_fields_at_every_object_level(self):
        _checkpoint_id, serialized, _key = checkpoint(authorization())
        payload = json.loads(serialized)
        for duplicated in (
            serialized.replace('"createdAtMs": 1', '"createdAtMs": 1, "createdAtMs": 1'),
            serialized.replace('"fileCount": 1', '"fileCount": 1, "fileCount": 1'),
            serialized.replace('"generation": 0', '"generation": 0, "generation": 0'),
        ):
            with self.subTest(duplicated=duplicated):
                self.assert_checkpoint_unresolved(payload, serialized=duplicated)

    def test_complete_checkpoint_accepts_known_unknown_and_boundary_metadata_without_recovery_claim(self):
        _checkpoint_id, serialized, _key = checkpoint(authorization())
        payload = json.loads(serialized)
        for generation in (
            {"status": "known", "token": {"instanceEpoch": "é" * 80, "generation": 2**64 - 1}},
            {"status": "unknown", "reason": " observation unavailable "},
        ):
            changed = payload | {"workspaceGeneration": generation, "sessionSequence": 2**64 - 1,
                                 "createdAtMs": 2**63 - 1, "executionId": "execution/path", "modelRequestId": "request/path"}
            result = self.inventory(grants=[authorization()], checkpoints=[("recovery", json.dumps(changed))])
            with self.subTest(generation=generation):
                self.assertTrue(any(root.source_kind == "recoveryCheckpoint" for root in result.roots))
                self.assertIn("concurrentReferencesNotFenced", result.unresolved)
        maximum = descriptor() | {"snapshotSizeBytes": 2**64 - 1, "expandedSizeBytes": 2**64 - 1, "fileCount": 2**32 - 1}
        checkpoint_id, serialized, _key = checkpoint(authorization(), snapshot=maximum)
        result = self.inventory(grants=[authorization()], checkpoints=[(checkpoint_id, serialized)])
        self.assertTrue(any(root.source_kind == "recoveryCheckpoint" for root in result.roots))
    def test_empty_snapshot_has_no_byte_object(self):
        empty = {"generation": 0, "snapshotSha256": "", "snapshotSizeBytes": 0,
                 "expandedSizeBytes": 0, "fileCount": 0}
        result = self.inventory(owner=session(empty), grants=[authorization(empty)])
        self.assertEqual(result.roots, ())
        self.assertFalse(any("Invalid" in gap for gap in result.unresolved))

    def test_foreign_or_malformed_grant_is_unresolved_without_guessed_key(self):
        for field, value in (("workspaceId", "foreign"), ("sessionId", "foreign"),
                             ("agentRunId", "foreign"), ("sessionWorkspace", descriptor(True))):
            grant = authorization()
            grant.payload[field] = value
            with self.subTest(field=field):
                result = self.inventory(grants=[grant])
                self.assertEqual(len(result.roots), 1)
                self.assertIn("runAuthorizationInvalid:" + grant.id, result.unresolved)

    def test_head_key_must_match_its_session_generation_and_digest(self):
        for key in (snapshot_key(descriptor(2), "foreign"), "../outside", snapshot_key(descriptor(1))):
            owner = session()
            owner.workspaceStorageKey = key
            with self.subTest(key=key):
                result = self.inventory(owner=owner)
                self.assertEqual(result.roots, ())
                self.assertIn("sessionHeadInvalid", result.unresolved)

    def test_checkpoint_does_not_adopt_foreign_path_scope_or_authorization(self):
        grant = authorization()
        checkpoint_id, serialized, _key = checkpoint(grant)
        original = json.loads(serialized)
        for change in ({"sessionId": "foreign"}, {"agentRunId": "foreign"},
                       {"authorizationDigest": "sha256:" + "b" * 64}, {"checkpointId": "foreign"},
                       {"workspaceSnapshot": original["workspaceSnapshot"] | {"objectRef": "../outside"}}):
            with self.subTest(change=change):
                result = self.inventory(grants=[grant], checkpoints=[(checkpoint_id, json.dumps(original | change))])
                self.assertFalse(any(root.source_kind == "recoveryCheckpoint" for root in result.roots))
                self.assertIn("recoveryCheckpointInvalid:" + checkpoint_id, result.unresolved)
        result = self.inventory(grants=[grant], checkpoints=[(checkpoint_id, "{invalid")])
        self.assertIn("recoveryCheckpointInvalid:" + checkpoint_id, result.unresolved)

    def test_missing_checkpoint_authorization_remains_unresolved(self):
        checkpoint_id, serialized, _key = checkpoint(authorization())
        result = self.inventory(checkpoints=[(checkpoint_id, serialized)])
        self.assertIn("recoveryCheckpointInvalid:" + checkpoint_id, result.unresolved)

    def test_partial_observation_keeps_unknown_roots_explicit_and_does_not_mutate_records(self):
        owner, grant = session(), authorization()
        before = copy.deepcopy((vars(owner), grant.payload))
        result = self.inventory(owner=owner, grants=[grant], checkpoints=None)
        self.assertIn("runtimeCheckpointsUnavailable", result.unresolved)
        self.assertIn("concurrentReferencesNotFenced", result.unresolved)
        self.assertIn("inflightWorkspaceUploadsNotIndexed", result.unresolved)
        self.assertIn("historicalPinsNotIndexed", result.unresolved)
        self.assertIn("preparedCompletionsNotIndexed", result.unresolved)
        self.assertEqual((vars(owner), grant.payload), before)
        self.assertEqual(session_workspace_for_session(owner), descriptor(2))
class SnapshotProtectionPostgresTests(OperationFixture, TestCase):
    def setUp(self):
        super().setUp()
        self.session = create_session(workspace=self.workspace, owner=self.user, agent=self.agent)
        self.completed_run = AgentRun.objects.create(workspace=self.workspace, session=self.session,
            user=self.user, modelConfig=self.model, prompt="previous workspace", status="completed")
        create_agent_run_authorization(self.completed_run, execution_profile=PROFILE)
        self.set_head(descriptor())
        self.run = AgentRun.objects.create(workspace=self.workspace, session=self.session,
            user=self.user, modelConfig=self.model, prompt="protected workspace")
        create_agent_run_authorization(self.run, execution_profile=PROFILE)
        self.frozen_key = self.session.workspaceStorageKey
        self.set_head(descriptor(2, b"changed workspace"))

    def set_head(self, value):
        self.session.workspaceGeneration = value["generation"]
        self.session.workspaceStorageKey = snapshot_key(value, self.session.id).replace(
            "workspaces/workspace/", f"workspaces/{self.workspace.id}/", 1)
        self.session.workspaceSnapshotSha256 = value["snapshotSha256"]
        self.session.workspaceSnapshotSizeBytes = value["snapshotSizeBytes"]
        self.session.workspaceExpandedSizeBytes = value["expandedSizeBytes"]
        self.session.workspaceFileCount = value["fileCount"]
        self.session.workspaceLastAdvancedAgentRun = self.completed_run
        self.session.save(update_fields=["workspaceGeneration", "workspaceStorageKey", "workspaceSnapshotSha256",
            "workspaceSnapshotSizeBytes", "workspaceExpandedSizeBytes", "workspaceFileCount",
            "workspaceLastAdvancedAgentRun", "updatedAt"])

    def test_reads_current_head_and_original_authorization_without_treating_terminal_as_release(self):
        AgentRun.objects.filter(pk=self.run.id).update(status="completed")
        result = collect_session_workspace_protection(self.session.id)
        self.assertEqual({root.storage_key for root in result.roots},
                         {self.frozen_key, self.session.workspaceStorageKey})
        self.assertIn("concurrentReferencesNotFenced", result.unresolved)

    def test_reads_real_runtime_checkpoint_and_excludes_other_session_rows(self):
        checkpoint_id = "checkpoint:protected"
        value = descriptor()
        key = (f"workspaces/{self.workspace.id}/sessions/{self.session.id}/agent-runs/{self.run.id}/"
               f"execution-checkpoints/{hashlib.sha256(checkpoint_id.encode()).hexdigest()}/"
               f"{value['snapshotSha256'][7:]}.snapshot")
        payload = complete_checkpoint_payload(checkpoint_id, self.session.id, self.run.id, self.run.authorization.digest,
            {name: value[name] for name in value if name != "generation"} | {"objectRef": key})
        with connection.cursor() as cursor:
            cursor.execute("CREATE SCHEMA IF NOT EXISTS runtime")
            cursor.execute("CREATE TABLE IF NOT EXISTS runtime.checkpoints("
                "checkpoint_id text PRIMARY KEY, kind text NOT NULL, session_id text NOT NULL, turn_id text NOT NULL, "
                "status text NOT NULL, done_reason text, updated_at_ms bigint NOT NULL, payload_json text NOT NULL, "
                "wait_handoff_json text)")
            for record_id, scope in ((checkpoint_id, self.session.id), ("checkpoint:second", self.session.id),
                                     ("checkpoint:foreign", "foreign-session")):
                record_payload = copy.deepcopy(payload)
                record_payload["checkpointId"] = record_id
                record_payload["workspaceSnapshot"]["objectRef"] = key.replace(
                    hashlib.sha256(checkpoint_id.encode()).hexdigest(), hashlib.sha256(record_id.encode()).hexdigest())
                cursor.execute("INSERT INTO runtime.checkpoints(checkpoint_id,kind,session_id,turn_id,status,"
                    "updated_at_ms,payload_json) VALUES(%s,'recovery',%s,'turn','committed',1,%s)",
                    [record_id, scope, json.dumps(record_payload)])
            session_payload = copy.deepcopy(payload)
            session_payload["checkpointId"] = "checkpoint:session-workspace"
            session_payload["workspaceSnapshot"]["objectRef"] = "session-workspace:generation:1"
            cursor.execute("INSERT INTO runtime.checkpoints(checkpoint_id,kind,session_id,turn_id,status,"
                "updated_at_ms,payload_json) VALUES(%s,'recovery',%s,'turn','committed',1,%s)",
                [session_payload["checkpointId"], self.session.id, json.dumps(session_payload)])
        result = collect_session_workspace_protection(self.session.id)
        root = next(root for root in result.roots if root.source_kind == "recoveryCheckpoint"
                    and root.source_id == checkpoint_id)
        self.assertEqual((root.storage_key, root.source_id, root.agent_run_id), (key, checkpoint_id, self.run.id))
        self.assertFalse(any("foreign" in gap for gap in result.unresolved))
        self.assertNotIn("runtimeCheckpointsUnavailable", result.unresolved)
        self.assertEqual(sum(root.source_kind == "recoveryCheckpoint" for root in result.roots), 3)
        session_root = next(root for root in result.roots if root.source_id == "checkpoint:session-workspace")
        self.assertEqual(session_root.storage_key, self.frozen_key)
        limited = collect_session_workspace_protection(self.session.id, max_records=1)
        self.assertIn("runtimeCheckpointsTruncated", limited.unresolved)
        self.assertIn("runAuthorizationsTruncated", limited.unresolved)

    def test_malformed_persisted_checkpoints_are_unresolved_without_deletion_permission(self):
        value = descriptor()
        snapshot = {name: value[name] for name in value if name != "generation"}
        snapshot["objectRef"] = "session-workspace:generation:1"
        original = complete_checkpoint_payload("checkpoint", self.session.id, self.run.id,
            self.run.authorization.digest, snapshot)
        missing = {name: value for name, value in original.items() if name != "executionId"}
        empty_ref = {"objectRef": "", "snapshotSha256": "", "snapshotSizeBytes": 0,
                     "expandedSizeBytes": 0, "fileCount": 0}
        invalid = [missing, original | {"unexpected": True}, original | {"sessionSequence": 0},
            original | {"createdAtMs": -1}, original | {"workspaceGeneration": {"status": "known",
                "token": {"instanceEpoch": "fixture-host", "generation": -1}}},
            original | {"workspaceSnapshot": snapshot | {"unexpected": True}},
            original | {"workspaceSnapshot": empty_ref}]
        checkpoint_ids = []
        with connection.cursor() as cursor:
            cursor.execute("CREATE SCHEMA IF NOT EXISTS runtime")
            cursor.execute("CREATE TABLE IF NOT EXISTS runtime.checkpoints("
                "checkpoint_id text PRIMARY KEY, kind text NOT NULL, session_id text NOT NULL, turn_id text NOT NULL, "
                "status text NOT NULL, done_reason text, updated_at_ms bigint NOT NULL, payload_json text NOT NULL, "
                "wait_handoff_json text)")
            for index, payload in enumerate(invalid):
                checkpoint_id = f"{self.session.id}:invalid:{index}"
                checkpoint_ids.append(checkpoint_id)
                cursor.execute("INSERT INTO runtime.checkpoints(checkpoint_id,kind,session_id,turn_id,status,"
                    "updated_at_ms,payload_json) VALUES(%s,'recovery',%s,'turn','committed',1,%s)",
                    [checkpoint_id, self.session.id, json.dumps(payload | {"checkpointId": checkpoint_id})])
        output = io.StringIO()
        call_command("inspect_workspace_protection", session_id=self.session.id, stdout=output)
        observed = json.loads(output.getvalue())
        self.assertFalse(observed["deletionAuthorized"])
        self.assertEqual({root["storageKey"] for root in observed["knownRoots"]},
                         {self.frozen_key, self.session.workspaceStorageKey})
        self.assertFalse(any(root["sourceKind"] == "recoveryCheckpoint" for root in observed["knownRoots"]))
        for checkpoint_id in checkpoint_ids:
            self.assertIn("recoveryCheckpointInvalid:" + checkpoint_id, observed["unresolved"])
    def test_runtime_read_failure_is_explicit_and_leaves_callers_transaction_usable(self):
        def unavailable(execute, sql, params, many, context):
            if "FROM runtime.checkpoints" in sql:
                raise DatabaseError("injected unavailable Runtime source")
            return execute(sql, params, many, context)
        with connection.execute_wrapper(unavailable):
            result = collect_session_workspace_protection(self.session.id)
        self.assertIn("runtimeCheckpointsUnavailable", result.unresolved)
        self.assertEqual(Session.objects.get(pk=self.session.id).workspaceStorageKey, self.session.workspaceStorageKey)
        self.assertEqual({root.storage_key for root in result.roots},
                         {self.frozen_key, self.session.workspaceStorageKey})

    def test_collector_only_reads_database_and_never_opens_or_mutates_file_storage(self):
        with patch.object(default_storage, "open", side_effect=AssertionError("inventory read file bytes")), \
                patch.object(default_storage, "save", side_effect=AssertionError("inventory wrote storage")), \
                patch.object(default_storage, "delete", side_effect=AssertionError("inventory deleted storage")), \
                CaptureQueriesContext(connection) as captured:
            result = collect_session_workspace_protection(self.session.id)
        self.assertEqual(len(result.roots), 2)
        self.assertTrue(captured.captured_queries)
        for query in captured.captured_queries:
            sql = query["sql"].lstrip().upper()
            self.assertTrue(sql.startswith(("SELECT", "SAVEPOINT", "RELEASE SAVEPOINT", "ROLLBACK TO SAVEPOINT"))
                or (sql.startswith("DECLARE ") and " CURSOR FOR SELECT " in sql), query["sql"])
    def test_management_command_outputs_scoped_json_without_authorizing_deletion(self):
        output = io.StringIO()
        with CaptureQueriesContext(connection) as captured:
            call_command("inspect_workspace_protection", session_id=self.session.id, stdout=output)
        value = json.loads(output.getvalue())
        self.assertEqual(value["schema"], "workspace.snapshot_protection.v1")
        self.assertEqual(value["sessionId"], self.session.id)
        self.assertFalse(value["deletionAuthorized"])
        self.assertEqual({root["storageKey"] for root in value["knownRoots"]},
                         {self.frozen_key, self.session.workspaceStorageKey})
        self.assertIn("preparedCompletionsNotIndexed", value["unresolved"])
        for query in captured.captured_queries:
            sql = query["sql"].lstrip().upper()
            self.assertTrue(sql.startswith(("SELECT", "SAVEPOINT", "RELEASE SAVEPOINT", "ROLLBACK TO SAVEPOINT"))
                or (sql.startswith("DECLARE ") and " CURSOR FOR SELECT " in sql), query["sql"])

    def test_record_budget_is_closed_and_truncation_is_an_explicit_coverage_gap(self):
        for limit in (0, -1, True, 10001, 1.5):
            with self.subTest(limit=limit), self.assertRaises(ValueError):
                collect_session_workspace_protection(self.session.id, max_records=limit)
        result = collect_session_workspace_protection(self.session.id, max_records=1)
        self.assertIn("runAuthorizationsTruncated", result.unresolved)
        self.assertTrue(any(root.source_kind == "sessionHead" for root in result.roots))

    def test_command_rejects_missing_session_and_invalid_budget(self):
        with self.assertRaises(CommandError):
            call_command("inspect_workspace_protection", session_id="missing-session", stdout=io.StringIO())
        with self.assertRaises(CommandError):
            call_command("inspect_workspace_protection", session_id=self.session.id, max_records=0, stdout=io.StringIO())
class CoreRecoveryCheckpointCorpusTests(SimpleTestCase):
    def test_shared_core_corpus_matches_the_observer_wire_shape(self):
        cases = json.loads(core_fixture("recovery_checkpoints.json").read_text(encoding="utf-8"))
        self.assertTrue(cases)
        self.assertEqual(len({case["name"] for case in cases}), len(cases))
        for case in cases:
            with self.subTest(case=case["name"]):
                try:
                    parsed = _read_checkpoint(case["serialized"])
                except (TypeError, ValueError, RecursionError):
                    valid = False
                else:
                    valid = True
                self.assertEqual(valid, case["expectedValid"])
                if valid:
                    expected = json.loads(case["serialized"])
                    expected["workspaceSnapshot"].setdefault("objectRef", None)
                    self.assertEqual(parsed, expected)