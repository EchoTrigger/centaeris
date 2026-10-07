import asyncio
import json
from types import SimpleNamespace
from contextlib import asynccontextmanager
from unittest.mock import AsyncMock, Mock, patch

import httpx
from django.test import SimpleTestCase
from openai import APIStatusError

from .http import internal_model
from .model_adapter import ModelProviderError, encode_model_stream_event
from .model_adapter.common import provider_error
from . import model_adapter


class InternalModelStreamTests(SimpleTestCase):
    async def test_provider_http_408_keeps_timeout_reason_and_status(self):
        response = httpx.Response(408, request=httpx.Request("POST", "https://provider.example/v1/chat/completions"))
        sdk_error = APIStatusError("secret provider response", response=response, body={"error": "secret provider response"})
        other_error = RuntimeError("secret provider response")
        other_error.status_code = 408
        for error in (sdk_error, other_error):
            with self.subTest(error_type=type(error).__name__):
                mapped = provider_error(error)
                self.assertIsInstance(mapped, ModelProviderError)

                async def source():
                    raise mapped
                    yield

                chunks = [part async for part in internal_model._model_stream_response(source())]
                payload = json.loads(chunks[0].decode().split("data: ", 1)[1])
                self.assertEqual(payload, {
                    "schema": "api.model.stream.v1", "type": "error",
                    "reasonType": "provider_timeout", "httpStatus": 408,
                })
                self.assertNotIn(b"secret provider response", chunks[0])

    async def test_cancellation_log_failure_cannot_replace_original_cancellation(self):
        @asynccontextmanager
        async def attempt(model):
            yield

        for cancellation in (asyncio.CancelledError("cancelled by user"), GeneratorExit()):
            with self.subTest(cancellation_type=type(cancellation).__name__):
                async def provider_stream(*args):
                    yield encode_model_stream_event("delta", {"delta": "partial"})
                    raise cancellation

                record = AsyncMock(side_effect=RuntimeError("log unavailable"))
                chunks = []
                with (
                    patch.object(model_adapter.ModelConfig.objects, "get", return_value=Mock(provider_id="provider")),
                    patch.object(model_adapter, "async_model_attempt", attempt),
                    patch.object(model_adapter, "resolve_model_route", return_value=("openai-completions", "https://provider.example")),
                    patch.object(model_adapter, "stream_open_ai_completions", provider_stream),
                    patch.object(model_adapter, "record_model_run_cancellation_safe", new=record),
                ):
                    with self.assertRaises(type(cancellation)) as raised:
                        async for part in internal_model._model_stream_response(model_adapter.stream_model_async("run", "model", {})):
                            chunks.append(part)
                self.assertIs(raised.exception, cancellation)
                record.assert_awaited_once()
                self.assertEqual(chunks, [encode_model_stream_event("delta", {"delta": "partial"})])

    async def test_log_failure_cannot_replace_the_original_provider_configuration_failure(self):
        @asynccontextmanager
        async def reject_attempt(model):
            raise ModelProviderError("model_quota_domain_required")
            yield

        with (
            patch.object(model_adapter.ModelConfig.objects, "get", return_value=Mock(provider_id="provider")),
            patch.object(model_adapter, "async_model_attempt", reject_attempt),
            patch.object(model_adapter, "record_model_run", new=AsyncMock(side_effect=RuntimeError("log unavailable"))),
        ):
            chunks = [part async for part in internal_model._model_stream_response(model_adapter.stream_model_async("run", "model", {}))]
        self.assertIn(b'"reasonType":"model_quota_domain_required"', chunks[0])

    async def response(self, source):
        request = SimpleNamespace(
            read=lambda limit: b'{"agentRunId":"run"}',
            headers={"Accept": "text/event-stream"},
        )
        with (
            patch.object(internal_model, "_validate_model_run", new=AsyncMock(return_value="model")),
            patch.object(internal_model, "stream_model_async", return_value=source),
        ):
            return await internal_model.model_runs(request)

    async def test_configuration_failure_is_a_terminal_error_frame_without_secret_details(self):
        closed = []

        async def source():
            try:
                raise ModelProviderError("model_quota_domain_required")
                yield
            finally:
                closed.append(True)

        response = await self.response(source())
        chunks = [part async for part in response]
        self.assertEqual(closed, [True])
        self.assertEqual(len(chunks), 1)
        payload = json.loads(chunks[0].decode().split("data: ", 1)[1])
        self.assertEqual(payload, {
            "schema": "api.model.stream.v1", "type": "error",
            "reasonType": "model_quota_domain_required",
            "httpStatus": None,
        })

    async def test_partial_provider_failure_keeps_its_reason_and_unknown_errors_are_sanitized(self):
        for error, reason in (
            (ModelProviderError("provider_stream_interrupted"), "provider_stream_interrupted"),
            (RuntimeError("secret provider response"), "model_adapter_failed"),
            (ModelProviderError("secret provider response"), "model_adapter_failed"),
        ):
            with self.subTest(reason=reason):
                async def source():
                    yield encode_model_stream_event("delta", {"delta": "partial"})
                    raise error

                response = await self.response(source())
                chunks = [part async for part in response]
                self.assertEqual(len(chunks), 2)
                self.assertIn(b'"delta":"partial"', chunks[0])
                self.assertIn(reason.encode(), chunks[1])
                self.assertNotIn(b"secret provider response", chunks[1])

    async def test_cancellation_remains_cancellation_and_success_is_not_followed_by_error(self):
        async def cancelled():
            raise asyncio.CancelledError()
            yield

        response = await self.response(cancelled())
        with self.assertRaises(asyncio.CancelledError):
            [part async for part in response]

        async def completed():
            yield encode_model_stream_event("result", {"text": "done", "toolCalls": [], "usage": None})
            raise RuntimeError("log failure after success")

        response = await self.response(completed())
        chunks = []
        with self.assertRaises(RuntimeError):
            async for part in response:
                chunks.append(part)
        self.assertEqual(len(chunks), 1)
        self.assertIn(b'"type":"result"', chunks[0])
