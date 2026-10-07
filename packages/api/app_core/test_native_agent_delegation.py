"""Business inputs enter the resolved user's native persistent Agent branch."""
import hashlib
import json
from unittest.mock import patch

from asgiref.sync import sync_to_async
from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.test import Client, TransactionTestCase
from django.test.utils import CaptureQueriesContext
from django.db import connection
from django.utils import timezone

from . import models, test_agent_messages, test_agent_previews
from .agent_input_delivery import dispatch_agent_inputs
from .app_delegations import token_digest
from .assets import captured_input_fields
from .runtime_client import build_agent_run_start
from .test_agent_work import PROFILE


SCOPES = ["assistant:use", "messages:submit", "sessions:read", "artifacts:read"]


class NativeAgentDelegationTests(TransactionTestCase):
    serialized_rollback = True
    bind = test_agent_messages.AgentMessageTests.bind
    message_run = test_agent_messages.AgentMessageTests.message_run
    commit = test_agent_messages.AgentMessageTests.commit
    validate = test_agent_messages.AgentMessageTests.validate
    accepted_pair = test_agent_messages.AgentMessageTests.accepted_pair
    attachment = test_agent_previews.AgentPreviewTests.attachment
    artifact = test_agent_previews.AgentPreviewTests.artifact
    read_bytes = test_agent_previews.AgentPreviewTests.read_bytes

    def setUp(self):
        test_agent_messages.AgentMessageTests.setUp(self)
        self.root = self.agent
        self.root_session = self.bind()
        self.ordinary_work = self.work
        self.application = models.BusinessApplication.objects.create(
            name="Synthetic business app", created_by=self.user, status="active")
        self.token = "cwa_" + "a" * 43
        self.grant = self.make_grant(self.application, self.token)
        self.bearer = Client()
        resolved = self.api("post", f"/api/agents/{self.root.pk}/business-branches/resolve",
            {"businessUserId": "stable-external-user"})
        self.assertIn(resolved.status_code, (200, 201), resolved.content)
        self.branch = models.BusinessAgentBranch.objects.get(pk=resolved.json()["branchId"])
        self.agent, self.session = self.branch.agent, self.branch.session
        self.coordination, self.work = self.session, self.session
        self.binding_url = f"/api/agents/{self.agent.pk}/coordination-session"
        self.messages_url = f"/api/agents/{self.agent.pk}/messages"
        self.input_url = f"/api/agents/{self.agent.pk}/inputs"
        self.history_url = f"/api/agents/{self.agent.pk}/history"
        profile = patch("app_core.runtime_client.request_execution_profile", return_value=PROFILE)
        schedule = patch("app_core.runtime_client.schedule_agent_run_lifecycle", return_value="inserted")
        self.profile_rpc = profile.start()
        self.schedule_rpc = schedule.start()
        self.addCleanup(profile.stop)
        self.addCleanup(schedule.stop)

    def make_grant(self, application, token, *, scopes=None):
        return models.UserAppDelegation.objects.create(user=self.user, app=application,
            workspace=self.workspace, agent=self.root, membership_ref=self.membership.pk,
            scopes=scopes if scopes is not None else SCOPES, token_digest=token_digest(token), expires_at=None)

    def api(self, method, path, payload=None, *, token=None, branch_id=None, use_branch=True):
        options = {"HTTP_AUTHORIZATION": "Bearer " + (token or self.token)}
        if branch_id is not None:
            options["HTTP_X_CENTAERIS_BUSINESS_BRANCH_ID"] = branch_id
        elif use_branch and hasattr(self, "branch"):
            options["HTTP_X_CENTAERIS_BUSINESS_BRANCH_ID"] = self.branch.pk
        if payload is not None:
            options.update(data=json.dumps(payload), content_type="application/json")
        return getattr(self.bearer, method)(path, **options)

    def submit(self, input_id="stable-business-input", body="  Exact business input\n中文  ", *, token=None):
        return self.api("post", self.input_url,
            {"schema": "agent.input.submit.v1", "attachmentRefs": [], "inputId": input_id, "body": body}, token=token)

    def rotate(self):
        token = "cwa_" + "b" * 43
        models.UserAppDelegation.objects.filter(pk=self.grant.pk).update(
            token_digest=token_digest(token), credential_version=2)
        self.token = token

    def test_app_input_records_first_source_and_same_grant_replays_exact_fact(self):
        first = self.submit()
        self.assertEqual(first.status_code, 201, first.content)
        replay = self.submit()
        self.assertEqual(replay.status_code, 200, replay.content)
        self.assertEqual(first.json(), replay.json())
        fact = models.AgentInput.objects.get()
        self.assertEqual((fact.acting_app_id, fact.app_delegation_id, fact.credential_version),
            (self.application.pk, self.grant.pk, 1))
        self.assertEqual(fact.membership_ref, self.membership.pk)
        self.assertEqual(fact.body, "  Exact business input\n中文  ")
        self.assertIsNone(first.json()["input"]["read"])
        self.assertFalse(models.AgentRun.objects.exists())
        conflict = self.submit(body="Changed business input")
        self.assertEqual(conflict.status_code, 409, conflict.content)
        self.assertEqual(models.AgentInput.objects.count(), 1)

    def test_input_id_cannot_replay_another_browser_application_or_grant_source(self):
        payload = {"schema": "agent.input.submit.v1", "attachmentRefs": [], "inputId": "browser-owned", "body": "same"}
        self.assertEqual(self.client.post(self.input_url, data=json.dumps(payload),
            content_type="application/json").status_code, 201)
        self.assertEqual(self.submit("browser-owned", "same").status_code, 403)
        self.assertEqual(self.submit("app-owned", "same").status_code, 201)
        payload["inputId"] = "app-owned"
        self.assertEqual(self.client.post(self.input_url, data=json.dumps(payload),
            content_type="application/json").status_code, 403)
        another_app = models.BusinessApplication.objects.create(
            name="Other synthetic app", created_by=self.user, status="active")
        other_token = "cwa_" + "c" * 43
        self.make_grant(another_app, other_token)
        self.assertEqual(self.submit("app-owned", "same", token=other_token).status_code, 404)
        another_token = "cwa_" + "d" * 43
        self.make_grant(self.application, another_token)
        self.assertEqual(self.submit("app-owned", "same", token=another_token).status_code, 403)
        self.assertEqual(models.AgentInput.objects.count(), 2)
        browser_fact = models.AgentInput.objects.get(input_id="browser-owned")
        self.assertEqual((browser_fact.acting_app_id, browser_fact.app_delegation_id,
            browser_fact.credential_version), (None, None, None))

    def test_rotation_allows_same_grant_replay_without_rewriting_original_version(self):
        first = self.submit()
        self.assertEqual(first.status_code, 201, first.content)
        old_token = self.token
        self.rotate()
        self.assertEqual(self.submit(token=old_token).status_code, 401)
        replay = self.submit()
        self.assertEqual(replay.status_code, 200, replay.content)
        self.assertEqual(replay.json(), first.json())
        self.assertEqual(models.AgentInput.objects.get().credential_version, 1)

    def test_rotation_and_revocation_after_authentication_are_rechecked_before_acceptance(self):
        from .agent_inputs import accept_agent_input
        def rotated(*args, **kwargs):
            self.rotate()
            return accept_agent_input(*args, **kwargs)
        with patch("app_core.http.agent_inputs.accept_agent_input", side_effect=rotated):
            response = self.submit()
        self.assertEqual(response.status_code, 401, response.content)
        self.assertFalse(models.AgentInput.objects.exists())
        def revoked(*args, **kwargs):
            models.UserAppDelegation.objects.filter(pk=self.grant.pk).update(revoked_at=timezone.now())
            return accept_agent_input(*args, **kwargs)
        with patch("app_core.http.agent_inputs.accept_agent_input", side_effect=revoked):
            response = self.submit()
        self.assertEqual(response.status_code, 403, response.content)
        self.assertFalse(models.AgentInput.objects.exists())
        self.assertFalse(models.AgentRun.objects.exists())

    def test_application_source_claims_are_server_only_and_browser_auth_remains_distinct(self):
        payload = {"schema": "agent.input.submit.v1", "attachmentRefs": [], "inputId": "claimed-source", "body": "Rejected"}
        for field, value in (("actingAppId", self.application.pk), ("appDelegationId", self.grant.pk),
                             ("credentialVersion", 1), ("userId", str(self.user.pk)),
                             ("businessUserId", "another-business-user")):
            response = self.api("post", self.input_url, {**payload, field: value})
            self.assertEqual(response.status_code, 400, response.content)
        response = self.client.post(self.input_url, data=json.dumps(payload), content_type="application/json",
            HTTP_AUTHORIZATION="Bearer " + self.token)
        self.assertEqual(response.status_code, 400, response.content)
        browser = Client(enforce_csrf_checks=True)
        browser.force_login(self.user)
        response = browser.post(self.input_url, data=json.dumps(payload), content_type="application/json")
        self.assertEqual(response.status_code, 403, response.content)
        self.assertFalse(models.AgentInput.objects.exists())

    def test_native_app_reads_same_coordination_input_message_and_uptake_history_as_owner(self):
        accepted = self.submit()
        self.assertEqual(accepted.status_code, 201, accepted.content)
        fact = models.AgentInput.objects.get()
        self.root.model_config = models.ModelConfig.objects.create(displayName="Synthetic uptake policy")
        self.root.save(update_fields=["model_config"])
        run = dispatch_agent_inputs(self.agent.pk)
        uptake = self.commit(run, "model_request_started", {"requestId": "native-main-one", "purpose": "main",
            "observations": [{"kind": "input_uptake", "inputIds": [fact.input_id]}]})
        models.SessionEvent.objects.filter(pk=uptake.pk).update(projects_to_agent_run_stream=False)
        self.accepted_pair(run)
        for path in (self.binding_url, self.input_url, self.history_url, self.messages_url):
            with self.subTest(path=path), CaptureQueriesContext(connection) as queries:
                delegated = self.api("get", path)
            self.assertEqual(delegated.status_code, 200, delegated.content)
            self.assertEqual(delegated.json(), self.client.get(path).json())
            self.assertFalse(any(query["sql"].lstrip().upper().startswith(("INSERT", "UPDATE", "DELETE"))
                for query in queries))
        read = self.api("get", self.input_url).json()["inputs"][0]["read"]
        self.assertEqual(read, {"agentRunId": run.pk, "eventId": uptake.pk,
            "requestId": "native-main-one", "createdAtMs": uptake.createdAtMs})
        self.assertEqual([item["kind"] for item in self.api("get", self.history_url).json()["items"]],
            ["input", "message"])

    def test_scopes_and_current_revocation_guard_all_app_reads_without_deleting_history(self):
        self.assertEqual(self.submit().status_code, 201)
        read_token = "cwa_" + "e" * 43
        self.make_grant(self.application, read_token, scopes=["assistant:use"])
        self.assertEqual(self.api("get", self.binding_url, token=read_token).status_code, 200)
        for path in (self.input_url, self.history_url, self.messages_url):
            self.assertEqual(self.api("get", path, token=read_token).status_code, 403)
        self.assertEqual(self.submit(token=read_token).status_code, 403)
        models.UserAppDelegation.objects.filter(pk=self.grant.pk).update(revoked_at=timezone.now())
        for path in (self.binding_url, self.input_url, self.history_url, self.messages_url):
            self.assertEqual(self.api("get", path).status_code, 403)
            self.assertEqual(self.client.get(path).status_code, 200)
        self.assertEqual(self.submit("later-input").status_code, 403)
        self.assertEqual(models.AgentInput.objects.count(), 1)

    def test_grant_is_exact_agent_and_cannot_mutate_coordination_or_owner_configuration(self):
        other = models.Agent.objects.create(workspace=self.workspace, owner=self.user, name="Another private Agent")
        foreign = models.Agent.objects.create(workspace=self.workspace, owner=self.other, name="Another owner Agent")
        for agent in (other, foreign):
            for suffix in ("inputs", "history", "messages", "coordination-session"):
                self.assertEqual(self.api("get", f"/api/agents/{agent.pk}/{suffix}").status_code, 404)
            response = self.api("post", f"/api/agents/{agent.pk}/inputs",
                {"schema": "agent.input.submit.v1", "attachmentRefs": [], "inputId": "foreign", "body": "rejected"})
            self.assertEqual(response.status_code, 404, response.content)
        listing = self.api("get", f"/api/workspaces/{self.workspace.pk}/agents")
        self.assertEqual(listing.status_code, 200, listing.content)
        self.assertEqual([agent["id"] for agent in listing.json()["agents"]], [self.agent.pk])
        self.assertEqual(self.api("post", self.binding_url, {}).status_code, 401)
        self.assertEqual(self.api("patch", f"/api/agents/{self.agent.pk}", {"instructions": "forged"}).status_code, 401)
        self.assertEqual(self.api("get", f"/api/agents/{self.agent.pk}/model-settings").status_code, 401)
        self.assertFalse(models.AgentInput.objects.exists())

    def test_native_app_requires_resolved_subject_and_cannot_read_root_or_sibling(self):
        resolved = self.api("post", f"/api/agents/{self.root.pk}/business-branches/resolve",
            {"businessUserId": "second-external-user"}, use_branch=False)
        self.assertIn(resolved.status_code, (200, 201), resolved.content)
        sibling = resolved.json()
        for agent_id in (self.root.pk, sibling["agentId"]):
            for suffix in ("inputs", "history", "messages", "coordination-session"):
                response = self.api("get", f"/api/agents/{agent_id}/{suffix}")
                self.assertEqual(response.status_code, 404, response.content)
        response = self.api("get", self.history_url, branch_id=sibling["branchId"])
        self.assertEqual(response.status_code, 404, response.content)
        response = self.api("get", self.history_url, use_branch=False)
        self.assertEqual(response.status_code, 400, response.content)
        first = self.submit("subject-input", "First user's input")
        self.assertEqual(first.status_code, 201, first.content)
        second = self.api("post", f"/api/agents/{sibling['agentId']}/inputs",
            {"schema": "agent.input.submit.v1", "attachmentRefs": [], "inputId": "subject-input", "body": "Second user's input"},
            branch_id=sibling["branchId"])
        self.assertEqual(second.status_code, 201, second.content)
        self.assertEqual(models.AgentInput.objects.count(), 2)
        history = self.api("get", f"/api/agents/{sibling['agentId']}/history", branch_id=sibling["branchId"])
        self.assertEqual(history.status_code, 200, history.content)
        self.assertEqual([item["input"]["body"] for item in history.json()["items"]], ["Second user's input"])

    def test_replaced_membership_does_not_revive_app_input_or_history_access(self):
        self.assertEqual(self.submit().status_code, 201)
        self.membership.delete()
        models.WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        self.assertEqual(self.submit("after-rejoin").status_code, 403)
        self.assertEqual(self.api("get", self.history_url).status_code, 403)
        self.assertEqual(models.AgentInput.objects.count(), 1)

    def test_managed_grant_keeps_ordinary_session_api_without_coordination_input_access(self):
        definition = models.AgentDefinition.objects.create(workspace=self.workspace,
            created_by=self.user, name="Managed assistant", availability_scope="workspace")
        version = models.AgentDefinitionVersion.objects.create(definition=definition, version=1,
            name=definition.name, published_by=self.user)
        definition.published_version = version
        definition.save(update_fields=["published_version"])
        agent = models.Agent.objects.create(workspace=self.workspace, owner=self.user,
            definition=definition, name=version.name)
        session = models.Session.objects.create(workspace=self.workspace, owner=self.user, agent=agent)
        models.AgentCoordinationSession.objects.create(agent=agent, session=session)
        managed_token = "cwa_" + "f" * 43
        models.UserAppDelegation.objects.create(user=self.user, app=self.application,
            workspace=self.workspace, definition=definition, membership_ref=self.membership.pk,
            scopes=SCOPES, token_digest=token_digest(managed_token), expires_at=None)
        for suffix in ("inputs", "history", "messages", "coordination-session"):
            response = self.api("get", f"/api/agents/{agent.pk}/{suffix}", token=managed_token)
            self.assertEqual(response.status_code, 403, response.content)
        response = self.api("post", f"/api/agents/{agent.pk}/inputs",
            {"schema": "agent.input.submit.v1", "attachmentRefs": [], "inputId": "managed-input", "body": "Rejected"},
            token=managed_token)
        self.assertEqual(response.status_code, 403, response.content)
        self.assertFalse(models.AgentInput.objects.exists())

    def test_native_app_cannot_use_ordinary_session_creation_or_message_admission(self):
        base = f"/api/workspaces/{self.workspace.pk}"
        self.assertEqual(self.api("post", base + "/sessions",
            {"agentId": self.agent.pk, "operationId": "native-created-session"}).status_code, 403)
        model = models.ModelConfig.objects.create(displayName="Synthetic ordinary model")
        response = self.api("post", base + f"/sessions/{self.ordinary_work.pk}/messages",
            {"operationId": "native-ordinary-message", "text": "Rejected", "modelConfigRef": model.pk})
        self.assertEqual(response.status_code, 403, response.content)
        self.assertFalse(models.AgentRun.objects.exists())
        self.assertFalse(models.HostedOperationReceipt.objects.exists())

    def test_accepted_app_input_dispatches_as_owner_native_agent_after_token_revoke(self):
        accepted = self.submit()
        self.assertEqual(accepted.status_code, 201, accepted.content)
        models.UserAppDelegation.objects.filter(pk=self.grant.pk).update(revoked_at=timezone.now())
        selected = models.ModelConfig.objects.create(displayName="Synthetic owner policy")
        self.root.model_config = selected
        self.root.save(update_fields=["model_config"])
        run = dispatch_agent_inputs(self.agent.pk)
        self.assertIsNotNone(run)
        self.assertEqual((run.user_id, run.acting_app_id, run.app_delegation_id), (self.user.pk, None, None))
        start = build_agent_run_start(run)
        self.assertEqual(start["nativeCoordinationSessionId"], self.session.pk)
        self.assertEqual(start["initialInput"], {"type": "userInput",
            "inputId": "stable-business-input", "message": accepted.json()["input"]["body"], "attachmentRefs": []})
        self.assertEqual(models.AgentInput.objects.count(), 1)
        self.assertEqual(models.AgentRun.objects.count(), 1)
        self.assertEqual(self.schedule_rpc.call_count, 1)
        self.assertEqual(self.api("get", self.input_url).status_code, 403)
        self.assertEqual(self.client.get(self.input_url).status_code, 200)

    def child_work(self):
        from .agent_work import materialize_work_request, work_contract_digest
        run = self.message_run(self.session)
        args = {"objective": "Synthetic business work", "session_refs": [], "file_refs": []}
        self.commit(run, "tool_call", {"toolName": "dispatch_work", "providerId": "workspace.agent_work",
            "toolContractDigest": work_contract_digest(), "normalizedInput": args}, call_id="business-work")
        result = self.commit(run, "tool_result", {"toolName": "dispatch_work",
            "resultState": "successWithOutput", "modelContent": "validated"}, call_id="business-work")
        with patch("app_core.agent_work.request_execution_profile", return_value=PROFILE), \
             patch("app_core.agent_work.schedule_agent_run_lifecycle", return_value="inserted"):
            response, created = materialize_work_request(result.pk)
        self.assertTrue(created)
        return models.AgentRun.objects.get(pk=response["operation"]["agentRunId"])

    def test_native_app_opens_only_the_exact_files_in_a_committed_agent_message(self):
        item, link, event, path, content = self.attachment()
        response = self.api("get", path)
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json(), self.client.get(path).json())
        file = response.json()["files"][0]
        self.assertEqual(self.read_bytes(self.api("get", file["previewUrl"])), content)
        self.assertEqual(self.read_bytes(self.api("get", file["downloadUrl"])), content)
        _, other_link, _, _, _ = self.attachment("unreferenced.txt")
        self.assertEqual(self.api("get", file["downloadUrl"].replace(link.pk, other_link.pk)).status_code, 404)
        self.assertEqual(self.api("get", path.replace(event.pk, "unknown-message")).status_code, 404)
        models.UserLibraryObject.objects.filter(pk=item.pk).update(contentGeneration=2)
        self.assertEqual(self.api("get", path).status_code, 409)
        self.assertEqual(self.api("get", file["downloadUrl"]).status_code, 409)

    def test_native_app_child_outputs_require_real_work_binding_and_original_artifact_session(self):
        child = self.child_work()
        artifact = self.artifact(child)
        response = self.api("get", f"/api/sessions/{child.session_id}/preview")
        self.assertEqual(response.status_code, 200, response.content)
        output = response.json()["outputs"][0]
        self.assertEqual(output["objectRef"], artifact.pk)
        self.assertEqual(self.read_bytes(self.api("get", output["downloadUrl"])), b"Published output")
        ordinary_artifact = self.artifact(self.message_run(self.ordinary_work), "ordinary-output.txt")
        self.assertEqual(self.api("get", f"/api/sessions/{self.ordinary_work.pk}/preview").status_code, 404)
        substituted = output["downloadUrl"].replace(artifact.pk, ordinary_artifact.pk)
        self.assertEqual(self.api("get", substituted).status_code, 404)
        models.AgentWorkSession.objects.filter(session_id=child.session_id).delete()
        self.assertEqual(self.api("get", f"/api/sessions/{child.session_id}/preview").status_code, 404)
        self.assertEqual(self.api("get", output["downloadUrl"]).status_code, 404)

    def test_sibling_subject_cannot_read_message_files_or_real_child_output_bytes(self):
        resolved = self.api("post", f"/api/agents/{self.root.pk}/business-branches/resolve",
            {"businessUserId": "file-isolated-subject"}, use_branch=False)
        self.assertIn(resolved.status_code, (200, 201), resolved.content)
        branch_id = resolved.json()["branchId"]
        _, _, _, path, _ = self.attachment()
        metadata = self.api("get", path)
        self.assertEqual(metadata.status_code, 200, metadata.content)
        file = metadata.json()["files"][0]
        child = self.child_work()
        self.artifact(child)
        preview_path = f"/api/sessions/{child.session_id}/preview"
        preview = self.api("get", preview_path)
        self.assertEqual(preview.status_code, 200, preview.content)
        output = preview.json()["outputs"][0]
        for url in (path, file["previewUrl"], file["downloadUrl"], preview_path,
                    output["previewUrl"], output["downloadUrl"]):
            with self.subTest(url=url):
                self.assertEqual(self.api("get", url, branch_id=branch_id).status_code, 404)

    def test_file_scope_and_grant_revocation_block_metadata_and_bytes_without_library_access(self):
        _, _, _, path, content = self.attachment()
        file = self.client.get(path).json()["files"][0]
        limited_token = "cwa_" + "g" * 43
        self.make_grant(self.application, limited_token,
            scopes=["assistant:use", "messages:submit", "sessions:read"])
        for url in (path, file["previewUrl"], file["downloadUrl"]):
            self.assertEqual(self.api("get", url, token=limited_token).status_code, 403)
        models.UserAppDelegation.objects.filter(pk=self.grant.pk).update(revoked_at=timezone.now())
        for url in (path, file["previewUrl"], file["downloadUrl"]):
            self.assertEqual(self.api("get", url).status_code, 403)
        self.assertEqual(self.read_bytes(self.client.get(file["downloadUrl"])), content)
        self.assertEqual(self.api("get", "/api/library").status_code, 401)

    def test_native_file_access_preserves_current_source_acl(self):
        self.membership.role = "member"
        self.membership.save(update_fields=["role"])
        content = b"Synthetic authorized source"
        key = default_storage.save("native-fixture/" + self.agent.pk + "/source.txt", ContentFile(content))
        source = models.Source.objects.create(workspace=self.workspace, sourceType="fileTree",
            name="Synthetic Source", status="ready", createdBy=self.other)
        item = models.SourceObject.objects.create(workspace=self.workspace, source=source, objectType="file",
            displayPath="source.txt", displayName="source.txt", contentType="text/plain", sizeBytes=len(content),
            sha256="sha256:" + hashlib.sha256(content).hexdigest(), storageKey=key, status="ready", contentGeneration=1)
        group = models.WorkspaceGroup.objects.create(workspace=self.workspace, name="Native readers", createdBy=self.other)
        group.members.add(self.membership)
        source_grant = models.SourceGrant.objects.create(workspace=self.workspace, source=source,
            workspaceGroup=group, createdBy=self.other)
        link = models.SessionAssetLink.objects.create(workspace=self.workspace, session=self.session,
            sourceObject=item, attachedBy=self.user, capturedDisplayName=item.displayName,
            capturedContentType=item.contentType, **captured_input_fields(item))
        event = self.accepted_pair(self.message_run(self.session), file_refs=[link.pk])
        path = f"/api/agents/{self.agent.pk}/messages/{event.pk}/files"
        response = self.api("get", path)
        self.assertEqual(response.status_code, 200, response.content)
        file = response.json()["files"][0]
        self.assertEqual(self.read_bytes(self.api("get", file["downloadUrl"])), content)
        source_grant.delete()
        for url in (path, file["previewUrl"], file["downloadUrl"]):
            self.assertEqual(self.api("get", url).status_code, 404)

    def test_message_bytes_recheck_frozen_credential_version_at_actual_storage_open(self):
        _, _, _, path, _ = self.attachment()
        file = self.client.get(path).json()["files"][0]
        from .http.storage_stream import stored_file_response
        async def rotate_before_open(*args, **kwargs):
            await sync_to_async(self.rotate, thread_sensitive=True)()
            return await stored_file_response(*args, **kwargs)
        with patch("app_core.http.agent_previews.stored_file_response", side_effect=rotate_before_open), \
             patch("app_core.http.agent_previews.default_storage.open") as storage_open:
            response = self.api("get", file["downloadUrl"])
        self.assertEqual(response.status_code, 409, response.content)
        self.assertFalse(response.streaming)
        storage_open.assert_not_called()
