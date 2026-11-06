"""Gateway calls need credentials, not an application-owned supplier quota."""
import json
import threading
from unittest.mock import patch

import httpx
from django.contrib.auth import get_user_model
from django.test import TestCase
from openai import OpenAI

from .credentials import encrypt_credential_secret
from .models import ModelConfig, ModelProvider, ProviderCredential
from .model_adapter import call_model, test_model_config as probe_model
from .model_adapter.common import ModelProviderError


def configure_gateway(test):
    test.admin = get_user_model().objects.create_superuser(
        username="gateway@example.com", password="password")
    test.provider = ModelProvider.objects.create(
        displayName="Gateway", api="openai-completions", apiBase="https://models.example.com/v1")
    ProviderCredential.objects.create(
        provider=test.provider, displayName="Test credential",
        encryptedSecret=encrypt_credential_secret("test-secret"),
        createdBy=test.admin, updatedBy=test.admin)
    test.model = ModelConfig.objects.create(
        displayName="test", provider=test.provider, modelName="test",
        resolvedApi=test.provider.api, resolvedApiBase=test.provider.apiBase)


class ModelGatewayTests(TestCase):
    setUp = configure_gateway

    def sdk_client(self, handler):
        transport = httpx.MockTransport(handler)
        def create(**options):
            return OpenAI(**options, http_client=httpx.Client(transport=transport))
        return patch("app_core.model_adapter.openai_completions.OpenAI", side_effect=create)

    def success(self):
        return httpx.Response(200, json={
            "id": "test", "object": "chat.completion", "created": 1, "model": "test",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "hello"},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}})

    def test_credential_without_quota_configuration_reaches_gateway(self):
        requests = []
        class UnreadBody(httpx.SyncByteStream):
            def __iter__(self):
                raise AssertionError("probe waited for the provider body")

        def handle(request):
            requests.append(request)
            return httpx.Response(200, stream=UnreadBody())

        with self.sdk_client(handle):
            self.assertEqual(probe_model(self.model), 200)
        self.assertEqual(len(requests), 1)
        self.assertEqual(requests[0].url.path, "/v1/chat/completions")
        self.assertLessEqual(requests[0].extensions["timeout"]["read"], 12)
        payload = json.loads(requests[0].content)
        self.assertEqual(payload["messages"], [{"role": "user", "content": "Reply OK."}])
        self.assertTrue(payload["stream"])
        self.assertNotIn("reasoning_effort", payload)

    def test_gateway_failure_preserves_retry_after_without_local_retry_or_cooldown(self):
        for status in (429, 503):
            requests = []
            def handle(request):
                requests.append(request)
                if len(requests) == 1:
                    return httpx.Response(status, headers={"Retry-After": "3600"},
                                          json={"error": {"message": "busy", "type": "busy"}})
                return self.success()
            with self.subTest(status=status), self.sdk_client(handle):
                with self.assertRaises(ModelProviderError) as caught:
                    probe_model(self.model)
                self.assertEqual((caught.exception.httpStatus, caught.exception.retryAfter), (status, "3600"))
                self.assertEqual(len(requests), 1)
                self.assertEqual(probe_model(self.model), 200)
                self.assertEqual(len(requests), 2)

    def test_cancelled_request_does_not_start_provider(self):
        cancelled = threading.Event()
        cancelled.set()
        with self.sdk_client(lambda request: self.fail("cancelled call reached provider")):
            with self.assertRaisesRegex(ModelProviderError, "model_run_cancelled"):
                call_model(self.model, {}, cancelled)
