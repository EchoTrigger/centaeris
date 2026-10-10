"""Public business chats use product resources and retained authority."""
import json
from unittest.mock import patch
from asgiref.sync import async_to_sync

from django.core.files.uploadedfile import SimpleUploadedFile
from django.test import SimpleTestCase, TransactionTestCase

from . import models
from .test_business_agent_branches import BusinessBranchFixture


class ChatApiTests(BusinessBranchFixture, TransactionTestCase):
    serialized_rollback = True
    def create(self, subject="customer-42", **kwargs):
        return self.api("post", "/api/v1/chats", {
            "agentId": self.root.pk, "businessUserId": subject}, **kwargs)

    def send_message(self, chat, key="request-1", text="Hello", files=None, token=None):
        with patch("app_core.agent_inputs._dispatch_after_commit"):
            return self.bearer.post(f"/api/v1/chats/{chat}/messages",
                data=json.dumps({"text": text, "fileIds": files or []}), content_type="application/json",
                HTTP_AUTHORIZATION="Bearer " + (token or self.token), HTTP_IDEMPOTENCY_KEY=key)

    def test_create_is_stable_and_message_round_trip_needs_only_chat_id(self):
        created = self.create()
        self.assertEqual(created.status_code, 201, created.content)
        chat = created.json()["data"]
        self.assertEqual(set(chat), {"id", "agentId", "businessUserId", "createdAt"})
        repeated = self.create()
        self.assertEqual(repeated.status_code, 200)
        self.assertEqual(repeated.json(), created.json())
        cid = chat["id"]
        accepted = self.send_message(cid)
        self.assertEqual(accepted.status_code, 201, accepted.content)
        message = accepted.json()["data"]
        self.assertEqual(set(message), {"id", "chatId", "author", "text", "fileIds", "createdAt", "readAt"})
        self.assertEqual((message["chatId"], message["author"], message["text"], message["readAt"]),
                         (cid, "user", "Hello", None))
        replay = self.send_message(cid)
        self.assertEqual(replay.status_code, 200, replay.content)
        self.assertEqual(replay.json(), accepted.json())
        self.assertEqual(self.send_message(cid, text="Changed").status_code, 409)
        lookup = self.api("get", f"/api/v1/chats/{cid}/messages/{message['id']}")
        self.assertEqual(lookup.json(), accepted.json())
        page = self.api("get", f"/api/v1/chats/{cid}/messages").json()
        self.assertEqual(page["data"], [message])
        self.assertEqual(set(page), {"data", "nextCursor", "hasMore"})

    def test_application_isolation_and_chat_scoped_message_and_cursor(self):
        a = self.create("A").json()["data"]["id"]
        b = self.create("B").json()["data"]["id"]
        msg = self.send_message(a).json()["data"]
        self.send_message(a, key="second", text="Second")
        page = self.api("get", f"/api/v1/chats/{a}/messages?limit=1").json()
        self.assertTrue(page["hasMore"])
        self.assertEqual(self.api("get", f"/api/v1/chats/{b}/messages/{msg['id']}").status_code, 404)
        self.assertEqual(self.api("get", f"/api/v1/chats/{b}/messages?cursor={page['nextCursor']}").status_code, 400)
        _, other_token = self.issue_grant(self.app())
        self.assertEqual(self.api("get", f"/api/v1/chats/{a}/messages", token=other_token).status_code, 404)

    def test_validation_does_not_accept_runtime_fields_or_missing_retry_identity(self):
        cid = self.create().json()["data"]["id"]
        for payload in ({"text": "hello", "sessionId": "caller-selected"}, {"text": "hello", "schema": "old"}):
            response = self.api("post", f"/api/v1/chats/{cid}/messages", payload)
            self.assertEqual(response.status_code, 400, response.content)
        response = self.api("post", f"/api/v1/chats/{cid}/messages", {"text": "hello"})
        self.assertEqual(response.status_code, 400)
        self.assertEqual(response.json()["error"], "idempotency_key_required")
        self.assertEqual(self.api("get", f"/api/v1/chats/{cid}/messages?unknown=1").status_code, 400)

    def test_revocation_and_rotation_recheck_current_authority(self):
        cid = self.create().json()["data"]["id"]
        old = self.token
        rotated = self.send("post", f"/api/account/app-delegations/{self.grant.pk}/rotate",
            {"expectedCredentialVersion": 1}, client=self.member_client)
        self.assertEqual(rotated.status_code, 200, rotated.content)
        self.token = rotated.json()["accessToken"]
        self.assertEqual(self.create().json()["data"]["id"], cid)
        self.assertEqual(self.send_message(cid, token=old).status_code, 401)
        self.assertEqual(self.send_message(cid).status_code, 201)
        self.send("delete", f"/api/account/app-delegations/{self.grant.pk}", client=self.member_client)
        self.assertEqual(self.api("get", f"/api/v1/chats/{cid}/messages").status_code, 403)

    def test_event_stream_replays_created_messages_and_resumes_with_opaque_id(self):
        cid = self.create().json()["data"]["id"]
        message = self.send_message(cid).json()["data"]
        response = self.api("get", f"/api/v1/chats/{cid}/events")
        self.assertEqual(response.status_code, 200, getattr(response, "content", b""))
        async def first_frame(response):
            stream = response.streaming_content.__aiter__()
            try:
                return (await anext(stream)).decode()
            finally:
                await stream.aclose()
        frame = async_to_sync(first_frame)(response)
        self.assertIn("event: message.created", frame)
        data = json.loads(next(line[6:] for line in frame.splitlines() if line.startswith("data: ")))
        self.assertEqual(data, message)
        cursor = next(line[4:] for line in frame.splitlines() if line.startswith("id: "))
        response.close()
        response = self.bearer.get(f"/api/v1/chats/{cid}/events",
            HTTP_AUTHORIZATION="Bearer " + self.token, HTTP_LAST_EVENT_ID=cursor)
        self.assertTrue(async_to_sync(first_frame)(response).startswith(": keepalive"))
        response.close()

    def test_file_upload_requires_explicit_scope_and_round_trips_bytes(self):
        cid = self.create().json()["data"]["id"]
        url = f"/api/v1/chats/{cid}/files"
        upload = lambda: self.bearer.post(url, {"files": SimpleUploadedFile("sample.txt", b"sample bytes")},
                                        HTTP_AUTHORIZATION="Bearer " + self.token)
        self.assertEqual(upload().status_code, 403)
        # A new explicit consent grants file upload; old credentials gain nothing.
        issued = self.send("post", "/api/account/app-delegations", {
            "appId": self.application["id"], "workspaceId": self.workspace.pk, "agentId": self.root.pk,
            "scopes": ["assistant:use", "messages:submit", "sessions:read", "artifacts:read", "attachments:write"],
            "expiresInSeconds": None}, client=self.member_client)
        self.assertEqual(issued.status_code, 201, issued.content)
        self.token = issued.json()["accessToken"]
        uploaded = upload()
        self.assertEqual(uploaded.status_code, 201, uploaded.content)
        file = uploaded.json()["data"][0]
        self.assertEqual(set(file), {"id", "name", "contentType", "sizeBytes"})
        accepted = self.send_message(cid, files=[file["id"]])
        self.assertEqual(accepted.status_code, 201, accepted.content)
        self.assertEqual(accepted.json()["data"]["fileIds"], [file["id"]])
        downloaded = self.api("get", url + "/" + file["id"])
        self.assertEqual(downloaded.status_code, 200)
        async def content(response):
            return b"".join([part async for part in response.streaming_content])
        self.assertEqual(async_to_sync(content)(downloaded), b"sample bytes")
        downloaded.close()
        other = self.create("other").json()["data"]["id"]
        self.assertEqual(self.api("get", f"/api/v1/chats/{other}/files/{file['id']}").status_code, 404)

    def test_read_update_after_disconnect_and_agent_output_use_one_message_shape(self):
        from .test_agent_messages import AgentMessageTests
        from .test_agent_input_delivery import AgentInputDeliveryTests
        cid = self.create().json()["data"]["id"]
        original = self.send_message(cid).json()["data"]
        async def take(response, count):
            stream = response.streaming_content.__aiter__()
            try:
                return [(await anext(stream)).decode() for _ in range(count)]
            finally:
                await stream.aclose()
        initial = self.api("get", f"/api/v1/chats/{cid}/events")
        frame = async_to_sync(take)(initial, 1)[0]
        cursor = next(line[4:] for line in frame.splitlines() if line.startswith("id: "))
        initial.close()
        branch = models.BusinessAgentBranch.objects.get(root_agent=self.root)
        self.user, self.agent, self.work, self.client = self.member, branch.agent, branch.session, self.member_client
        self.commit = AgentMessageTests.commit.__get__(self)
        self.validate = AgentMessageTests.validate.__get__(self)
        run = AgentMessageTests.message_run(self, branch.session)
        queue = models.AgentInputQueue.objects.create(agent_run=run, authorization_digest=run.authorization.digest)
        models.AgentInputDelivery.objects.create(input=models.AgentInput.objects.get(agent=branch.agent), queue=queue)
        AgentInputDeliveryTests.uptake(self, run, ["request-1"])
        AgentMessageTests.accepted_pair(self, run, body="Report complete")
        response = self.bearer.get(f"/api/v1/chats/{cid}/events",
            HTTP_AUTHORIZATION="Bearer " + self.token, HTTP_LAST_EVENT_ID=cursor)
        self.assertEqual(response.status_code, 200)
        frames = async_to_sync(take)(response, 2)
        response.close()
        self.assertIn("event: message.updated", frames[0])
        updated = json.loads(next(line[6:] for line in frames[0].splitlines() if line.startswith("data: ")))
        self.assertEqual(updated["id"], original["id"])
        self.assertIsNotNone(updated["readAt"])
        output = json.loads(next(line[6:] for line in frames[1].splitlines() if line.startswith("data: ")))
        self.assertEqual((output["author"], output["text"]), ("agent", "Report complete"))
        self.assertEqual(set(output), set(original))
        lookup = self.api("get", f"/api/v1/chats/{cid}/messages/{output['id']}")
        self.assertEqual(lookup.json()["data"], output)

    def test_published_assistant_has_isolated_persistent_users_and_server_model_policy(self):
        from .agent_input_delivery import dispatch_agent_inputs
        from .test_agent_definitions import PROFILE
        definition, _, issued = self.delegation()
        token = issued.json()["accessToken"]
        def create(user):
            return self.api("post", "/api/v1/chats", {
                "agentId": definition["id"], "businessUserId": user}, token=token)
        first = create("published-A")
        self.assertEqual(first.status_code, 201, first.content)
        a = first.json()["data"]["id"]
        self.assertEqual(create("published-A").json()["data"]["id"], a)
        b = create("published-B").json()["data"]["id"]
        self.assertNotEqual(a, b)
        accepted = self.send_message(a, token=token)
        self.assertEqual(accepted.status_code, 201, accepted.content)
        self.assertEqual(self.api("get", f"/api/v1/chats/{b}/messages", token=token).json()["data"], [])
        fact = models.AgentInput.objects.get(input_id="request-1")
        branch = models.BusinessAgentBranch.objects.get(agent=fact.agent)
        model = models.ModelConfig.objects.create(displayName="Platform default")
        configured = self.send("patch", f"/api/agents/{branch.root_agent_id}/model-settings",
            {"schema": "agent.model_settings.update.v1", "modelConfigRef": model.pk, "thinkingMode": None}, client=self.member_client)
        self.assertEqual(configured.status_code, 200, configured.content)
        with patch("app_core.runtime_client.request_execution_profile", return_value=PROFILE), \
             patch("app_core.runtime_client.schedule_agent_run_lifecycle", return_value="inserted"):
            run = dispatch_agent_inputs(branch.agent_id)
        self.assertEqual(run.modelConfig_id, model.pk)
        self.assertEqual(run.definition_version.definition_id, definition["id"])
        self.assertEqual(run.agent_instructions, run.definition_version.instructions)
        listing = self.api("get", self.base + "/agents", token=token).json()["agents"]
        self.assertEqual([item["id"] for item in listing], [branch.root_agent_id])
        # Generic definition transports cannot bypass a chat's binding.
        self.assertEqual(self.api("get", f"/api/sessions/{branch.session_id}", token=token).status_code, 404)
        from importlib import import_module
        from django.apps import apps
        from django.db import connection
        migration = import_module("app_core.migrations.0007_business_definition_instances")
        with self.assertRaisesRegex(RuntimeError, "published conversations"):
            migration.refuse_managed_conversation_loss(apps, connection.schema_editor())
        self.assertEqual(create("published-A").json()["data"]["id"], a)
        self.availability(definition, "none")
        self.assertEqual(self.api("get", f"/api/v1/chats/{a}/messages", token=token).status_code, 403)

    def test_deleted_published_owner_instance_blocks_existing_chat(self):
        from django.utils import timezone
        definition, _, issued = self.delegation()
        token = issued.json()["accessToken"]
        created = self.api("post", "/api/v1/chats", {
            "agentId": definition["id"], "businessUserId": "customer"}, token=token)
        self.assertEqual(created.status_code, 201, created.content)
        cid = created.json()["data"]["id"]
        models.Agent.objects.filter(definition_id=definition["id"], is_business_instance=False).update(
            status="deleted", deletedAt=timezone.now())
        self.assertEqual(self.api("get", f"/api/v1/chats/{cid}/messages", token=token).status_code, 404)
        self.assertEqual(self.send_message(cid, token=token).status_code, 404)

    def test_live_stream_closes_when_credential_rotates_between_frames(self):
        cid = self.create().json()["data"]["id"]
        self.send_message(cid)
        self.send_message(cid, key="second", text="Second")
        response = self.api("get", f"/api/v1/chats/{cid}/events")
        from asgiref.sync import sync_to_async
        async def consume():
            stream = response.streaming_content.__aiter__()
            try:
                await anext(stream)
                rotated = await sync_to_async(self.send)("post", f"/api/account/app-delegations/{self.grant.pk}/rotate",
                    {"expectedCredentialVersion": 1}, client=self.member_client)
                self.assertEqual(rotated.status_code, 200)
                with self.assertRaises(StopAsyncIteration):
                    await anext(stream)
            finally:
                await stream.aclose()
        async_to_sync(consume)()
        response.close()


class ChatOpenApiTests(SimpleTestCase):
    def test_generated_contract_describes_retry_header_file_upload_and_event_stream(self):
        from api.ninja_api import api
        paths = api.get_openapi_schema()["paths"]
        send = paths["/api/v1/chats/{chat_id}/messages"]["post"]
        self.assertTrue(any(p["in"] == "header" and p["name"] == "Idempotency-Key" for p in send["parameters"]))
        upload = paths["/api/v1/chats/{chat_id}/files"]["post"]
        self.assertIn("multipart/form-data", upload["requestBody"]["content"])
        stream = paths["/api/v1/chats/{chat_id}/events"]["get"]
        self.assertIn("text/event-stream", stream["responses"][200]["content"])
