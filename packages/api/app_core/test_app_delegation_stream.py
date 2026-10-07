import asyncio
import json
import time
from unittest.mock import AsyncMock, patch

from asgiref.sync import sync_to_async
from django.test import AsyncClient, TransactionTestCase

from app_core.models import AgentRun, ModelConfig
from app_core.test_app_delegations import AppDelegationSessionFixture


class AppDelegationHttpStreamTests(AppDelegationSessionFixture, TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        super().setUp()
        version = self.agent.definition.published_version
        self.run_record = AgentRun.objects.create(workspace=self.workspace, session=self.session, user=self.member,
            modelConfig=ModelConfig.objects.create(displayName="Fixture"), prompt="Fixture",
            definition_version=version, agent_instructions=version.instructions)

    async def _assert_idle_stream_closes(self, mutation, expected_status):
        waiting, cancelled = asyncio.Event(), asyncio.Event()
        async def blocked_read(*args, **kwargs):
            waiting.set()
            try:
                await asyncio.Event().wait()
            finally:
                cancelled.set()

        with patch("app_core.agent_run_stream._capture_signal_tail", AsyncMock(return_value="0-0")), \
             patch("app_core.agent_run_stream._load_live_text_state_async", AsyncMock(return_value=None)), \
             patch("app_core.agent_run_stream._redis_stream_client.xread", blocked_read):
            response = await AsyncClient().get(f"/api/sessions/{self.session.id}/agent-runs/{self.run_record.id}/events",
                headers={"Authorization": "Bearer " + self.token})
            self.assertEqual(response.status_code, 200)
            self.assertEqual(response["Content-Type"], "text/event-stream")
            iterator = response.streaming_content.__aiter__()
            pending = asyncio.create_task(anext(iterator))
            try:
                await asyncio.wait_for(waiting.wait(), 10)
                changed = await sync_to_async(mutation, thread_sensitive=True)()
                self.assertEqual(changed.status_code, expected_status, changed.content)
                committed_at = time.monotonic()
                with self.assertRaises(StopAsyncIteration):
                    await asyncio.wait_for(pending, timeout=14.5)
                self.assertLess(time.monotonic() - committed_at, 15)
                await asyncio.wait_for(cancelled.wait(), 2)
            finally:
                if not pending.done():
                    pending.cancel()
                    await asyncio.gather(pending, return_exceptions=True)
                await iterator.aclose()

    async def test_real_http_stream_closes_after_committed_grant_revocation_within_fifteen_seconds(self):
        await self._assert_idle_stream_closes(
            lambda: self.member_client.delete(f"/api/account/app-delegations/{self.grant.id}"), 204)

    async def test_real_http_stream_closes_after_credential_rotation_within_fifteen_seconds(self):
        await self._assert_idle_stream_closes(
            lambda: self.member_client.post(f"/api/account/app-delegations/{self.grant.id}/rotate",
                json.dumps({"expectedCredentialVersion": 1}), content_type="application/json"), 200)

    async def test_event_scope_is_required_before_stream_creation(self):
        from app_core.models import UserAppDelegation
        await sync_to_async(UserAppDelegation.objects.filter(pk=self.grant.id).update, thread_sensitive=True)(
            scopes=["sessions:read"])
        response = await AsyncClient().get(f"/api/sessions/{self.session.id}/agent-runs/{self.run_record.id}/events",
            headers={"Authorization": "Bearer " + self.token})
        self.assertEqual(response.status_code, 403, response.content)
        self.assertEqual(response.json(), {"error": "delegation_scope_forbidden"})
