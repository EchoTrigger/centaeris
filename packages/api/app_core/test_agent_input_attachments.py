"""Accepted attachment identities survive idle/active intake and replay."""
import json
from unittest.mock import patch

from django.core.files.uploadedfile import SimpleUploadedFile
from django.test import TransactionTestCase

from . import models, test_agent_input_delivery as fixture
from .deferred_input import resolved_input_storage
from .runtime_client import build_agent_run_start
from .http.agent_previews import _select_content, PreviewRejected
from django.http import HttpRequest


class AgentInputAttachmentTests(TransactionTestCase):
    serialized_rollback = True
    bind = fixture.AgentInputDeliveryTests.bind
    runtime = fixture.AgentInputDeliveryTests.runtime
    input_url = fixture.AgentInputDeliveryTests.input_url
    delivery = fixture.AgentInputDeliveryTests.delivery

    def setUp(self):
        fixture.AgentInputDeliveryTests.setUp(self)

    def upload(self, name="evidence.txt", content=None):
        content = content if content is not None else name.encode()
        response = self.client.post(f"/api/sessions/{self.session.pk}/uploads", {
            "files": [SimpleUploadedFile(name, content, "image/png" if name.endswith(".png") else "text/plain")]})
        self.assertEqual(response.status_code, 201, response.content)
        return response.json()["assets"][0]["id"]

    def submit(self, identity, body, refs):
        return self.client.post(self.input_url, data=json.dumps({"schema": "agent.input.submit.v1",
            "inputId": identity, "body": body, "attachmentRefs": refs}), content_type="application/json")

    def test_initial_attachment_is_captured_authorized_and_pure_attachment_input_is_accepted(self):
        unused = self.upload("unused.txt")
        attached = self.upload()
        with self.runtime():
            response = self.submit("file-initial", "", [attached])
        self.assertEqual(response.status_code, 201, response.content)
        self.assertEqual(response.json()["input"]["attachments"], [{"inputRef": attached,
            "displayName": "evidence.txt", "contentType": "text/plain"}])
        run = self.delivery("file-initial").queue.agent_run
        self.assertEqual(run.authorization.payload["messageAssetRefs"], [attached])
        self.assertEqual([item["inputRef"] for item in run.authorization.payload["assetRefs"]], [attached])
        self.assertNotIn(unused, json.dumps(run.authorization.payload))
        self.assertEqual(build_agent_run_start(run)["initialInput"]["attachmentRefs"], [attached])

    def test_active_input_keeps_run_signature_and_uses_only_its_captured_attachment_grant(self):
        with self.runtime():
            first = self.submit("text-initial", "first", [])
        self.assertEqual(first.status_code, 201, first.content)
        run = self.delivery("text-initial").queue.agent_run
        original = (run.authorization.digest, run.authorization.payload, run.authorization.signature)
        attached = self.upload("late.txt")
        with self.runtime(profile=AssertionError("active intake cannot create another Run")):
            response = self.submit("file-active", "late input", [attached])
        self.assertEqual(response.status_code, 201, response.content)
        self.assertEqual(self.delivery("file-active").queue_id, run.pk)
        run.authorization.refresh_from_db()
        self.assertEqual((run.authorization.digest, run.authorization.payload, run.authorization.signature), original)
        resolved, _ = resolved_input_storage(run, attached, run.authorization.digest)
        self.assertEqual(resolved["inputRef"], attached)
        self.assertEqual(models.AgentRun.objects.count(), 1)
        self.assertIsNone(response.json()["input"]["read"])

    def test_late_material_is_listed_and_bound_only_after_owned_input_delivery(self):
        from .material_access import bind_inputs, MaterialAccess
        from .material_identity import representation_id
        from .platform_mcp import execute_tool
        with self.runtime():
            self.assertEqual(self.submit("initial-text", "text only", []).status_code, 201)
        run = self.delivery("initial-text").queue.agent_run
        attached, undeclared = self.upload("late-material.txt"), self.upload("not-sent.txt")
        with self.runtime():
            response = self.submit("active-material", "", [attached])
        self.assertEqual(response.status_code, 201, response.content)
        capture = self.delivery("active-material").input.attachments[0]
        spec = "sha256:" + "b" * 64
        access = MaterialAccess(run, {}, spec, run.authorization.digest)
        with patch("app_core.platform_mcp.authorize_context", return_value=access):
            listed = execute_tool({}, "list_materials", {})
        self.assertEqual([item["inputRef"] for item in listed["items"]], [attached])
        bound = bind_inputs(run, run.authorization.digest, [{"inputRef": attached,
            "representationId": representation_id(capture["inputIdentity"], spec)}], spec)
        self.assertEqual(bound[0].resolved["inputRef"], attached)
        with self.assertRaisesMessage(Exception, "knowledge_input_not_authorized"):
            bind_inputs(run, run.authorization.digest, [{"inputRef": undeclared,
                "representationId": representation_id(capture["inputIdentity"], spec)}], spec)
        self.assertEqual(run.authorization.payload["assetRefs"], [])
        models.WorkspaceMembership.objects.filter(pk=run.membership_ref).delete()
        with self.assertRaisesMessage(Exception, "knowledge_input_binding_invalid"):
            bind_inputs(run, run.authorization.digest, [{"inputRef": attached,
                "representationId": representation_id(capture["inputIdentity"], spec)}], spec)

    def test_replay_preserves_capture_and_different_refs_conflict_without_inserting(self):
        a, b = self.upload("a.txt"), self.upload("b.txt")
        with patch("app_core.agent_input_delivery.dispatch_agent_inputs"):
            first = self.submit("same-input", "same", [a])
            same = self.submit("same-input", "same", [a])
            changed = self.submit("same-input", "same", [b])
        self.assertEqual((first.status_code, same.status_code, changed.status_code), (201, 200, 409))
        self.assertEqual(first.json(), same.json())
        self.assertEqual(models.AgentInput.objects.count(), 1)
        self.assertEqual(self.client.get(self.input_url).json()["inputs"], [first.json()["input"]])

    def test_foreign_unsorted_duplicate_unknown_and_empty_input_are_rejected(self):
        a, b = sorted([self.upload("a.txt"), self.upload("b.txt")])
        for body, refs in [("", []), ("text", [b, a]), ("text", [a, a]), ("text", ["foreign"]), ("text", [a] * 51)]:
            with self.subTest(body=body, refs=refs), patch("app_core.agent_input_delivery.dispatch_agent_inputs"):
                self.assertIn(self.submit("rejected", body, refs).status_code, [400, 403])
        self.assertFalse(models.AgentInput.objects.exists())

    def test_nine_files_are_accepted_with_one_exact_capture_per_reference(self):
        refs = sorted(self.upload(f"evidence-{index}.txt") for index in range(9))
        with self.runtime():
            response = self.submit("nine-files", "", refs)
        self.assertEqual(response.status_code, 201, response.content)
        self.assertEqual([item["inputRef"] for item in response.json()["input"]["attachments"]], refs)
        self.assertEqual(build_agent_run_start(self.delivery("nine-files").queue.agent_run)["initialInput"]["attachmentRefs"], refs)

    def test_run_capture_budget_retains_late_input_for_the_next_serialized_run(self):
        from copy import deepcopy
        from .agent_input_attachments import capture_attachments
        from .agent_input_delivery import dispatch_agent_inputs
        real = capture_attachments(self.user, self.session, [self.upload()])[0]
        def captures(prefix, count):
            return [{**deepcopy(real), "inputRef": f"{prefix}-{index:02}"} for index in range(count)]
        first, late = captures("first", 50), captures("late", 15)
        with self.runtime(), patch("app_core.agent_inputs.capture_attachments", return_value=first):
            self.assertEqual(self.submit("full-first", "", [item["inputRef"] for item in first]).status_code, 201)
        run = self.delivery("full-first").queue.agent_run
        original = deepcopy(run.authorization.payload), run.authorization.digest, run.authorization.signature
        with self.runtime(), patch("app_core.agent_inputs.capture_attachments", return_value=late):
            self.assertEqual(self.submit("deferred-late", "", [item["inputRef"] for item in late]).status_code, 201)
        fact = models.AgentInput.objects.get(input_id="deferred-late")
        self.assertEqual(fact.attachments, late)
        self.assertFalse(models.AgentInputDelivery.objects.filter(input=fact).exists())
        self.assertEqual(models.AgentRun.objects.count(), 1)
        run.authorization.refresh_from_db()
        self.assertEqual((run.authorization.payload, run.authorization.digest, run.authorization.signature), original)
        models.AgentRun.objects.filter(pk=run.pk).update(status="completed")
        with self.runtime():
            following = dispatch_agent_inputs(self.agent.pk)
        self.assertNotEqual(following.pk, run.pk)
        self.assertEqual(self.delivery("deferred-late").queue_id, following.pk)

    def test_accepted_and_draft_preview_bind_captured_identity_and_reject_changed_content(self):
        attached = self.upload()
        with patch("app_core.agent_input_delivery.dispatch_agent_inputs"):
            self.assertEqual(self.submit("preview-input", "", [attached]).status_code, 201)
        link = models.SessionAssetLink.objects.get(pk=attached)
        query = HttpRequest()
        query.GET = {}
        selected = _select_content(self.user, query, str(link.capturedContentGeneration),
            link.capturedSha256, "zh-CN", agent_id=self.agent.pk, input_id="preview-input",
            input_ref=attached, agent_attachment=True)
        self.assertEqual(selected[2], "evidence.txt")
        with self.assertRaises(PreviewRejected):
            _select_content(self.other, query, str(link.capturedContentGeneration), link.capturedSha256,
                "zh-CN", agent_id=self.agent.pk, input_id="preview-input", input_ref=attached, agent_attachment=True)
        models.UserLibraryObject.objects.filter(pk=link.userLibraryObject_id).update(contentGeneration=2)
        with self.assertRaises(PreviewRejected) as rejected:
            _select_content(self.user, query, str(link.capturedContentGeneration), link.capturedSha256,
                "zh-CN", agent_id=self.agent.pk, input_id="preview-input", input_ref=attached, agent_attachment=True)
        self.assertEqual(rejected.exception.status, 409)
        self.assertEqual(self.client.get(self.input_url).json()["inputs"][0]["attachments"][0]["displayName"], "evidence.txt")

    def test_explicit_forward_migration_preserves_existing_input_and_refuses_lossy_reversal(self):
        from django.db import connection
        from django.db.migrations.executor import MigrationExecutor
        with patch("app_core.agent_input_delivery.dispatch_agent_inputs"):
            self.assertEqual(self.submit("legacy", "  retained text\n", []).status_code, 201)
        try:
            executor = MigrationExecutor(connection)
            executor.migrate([("app_core", "0004_business_agent_branches")])
            executor = MigrationExecutor(connection)
            target = [("app_core", "0005_agent_input_attachments")]
            executor.migrate(target)
            historical = executor.loader.project_state(target).apps
            legacy = historical.get_model("app_core", "AgentInput").objects.get(input_id="legacy")
            self.assertEqual((legacy.body, legacy.sequence, legacy.membership_ref, legacy.attachments),
                ("  retained text\n", 1, self.membership.pk, []))
            # Current HTTP/model code must only run against the current schema.
            latest = MigrationExecutor(connection)
            latest.migrate(latest.loader.graph.leaf_nodes("app_core"))
            attached = self.upload()
            with patch("app_core.agent_input_delivery.dispatch_agent_inputs"):
                self.assertEqual(self.submit("captured", "", [attached]).status_code, 201)
            with self.assertRaisesMessage(ValueError, "agent_input_attachment_migration_cannot_discard_captures"):
                MigrationExecutor(connection).migrate([("app_core", "0004_business_agent_branches")])
        finally:
            latest = MigrationExecutor(connection)
            latest.migrate(latest.loader.graph.leaf_nodes("app_core"))
