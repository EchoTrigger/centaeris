"""Exercise failure facts through provider iteration and the hosted SSE boundary."""

import json
from unittest.mock import AsyncMock, Mock, patch

import anthropic
import httpx
import openai
from django.test import SimpleTestCase
from openai import APIStatusError

from .http.internal_model import _model_stream_response
from .model_adapter import encode_model_stream_event
from .model_adapter import anthropic_messages, openai_completions, openai_responses
from .model_adapter.common import ModelProviderError, provider_error


class ModelTransportErrorTests(SimpleTestCase):
    def test_anthropic_sdk_timeout_and_connection_keep_transport_categories(self):
        request = httpx.Request("POST", "https://synthetic.invalid/messages")
        for error, reason in (
            (anthropic.APITimeoutError(request), "provider_timeout"),
            (anthropic.APIConnectionError(request=request), "provider_unreachable"),
        ):
            with self.subTest(error_type=type(error).__name__):
                mapped = provider_error(error)
                self.assertIsInstance(mapped, ModelProviderError)
                self.assertEqual(mapped.reasonType, reason)
                self.assertIsNone(mapped.httpStatus)

    def test_openai_forbidden_is_authentication_failure_and_preserves_status(self):
        response = httpx.Response(403, request=httpx.Request("POST", "https://synthetic.invalid"))
        mapped = provider_error(APIStatusError("private body", response=response, body=None))
        self.assertEqual(mapped.reasonType, "provider_authentication_failed")
        self.assertEqual(mapped.httpStatus, 403)

    def test_provider_status_and_retry_after_remain_available_for_quota_accounting(self):
        request = httpx.Request("POST", "https://synthetic.invalid")
        for status, reason in (
            (408, "provider_timeout"),
            (429, "provider_rate_limited"),
            (503, "provider_unavailable"),
        ):
            for sdk in (openai, anthropic):
                with self.subTest(status=status, sdk=sdk.__name__):
                    response = httpx.Response(status, headers={"Retry-After": "3"}, request=request)
                    mapped = provider_error(sdk.APIStatusError("private body", response=response, body=None))
                    self.assertEqual(mapped.reasonType, reason)
                    self.assertEqual(mapped.httpStatus, status)
                    self.assertEqual(mapped.retryAfter, "3")

    async def frames_after_partial_output(self, module, error):
        if module is openai_completions:
            frame = {"choices": [{"delta": {"content": "partial"}}]}
            create_path = "chat.completions.create"
            client_factory = "async_open_ai_completions_client"
            request_builder = "build_open_ai_completions_request"
            stream_function = module.stream_open_ai_completions
        elif module is openai_responses:
            frame = {"type": "response.output_text.delta", "delta": "partial"}
            create_path = "responses.create"
            client_factory = "async_open_ai_responses_client"
            request_builder = "build_open_ai_responses_request"
            stream_function = module.stream_open_ai_responses
        else:
            frame = {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "partial"}}
            create_path = "messages.create"
            client_factory = "async_anthropic_messages_client"
            request_builder = "build_anthropic_messages_request"
            stream_function = module.stream_anthropic_messages

        async def provider_events():
            yield frame
            raise error

        client = Mock()
        owner = client
        parts = create_path.split(".")
        for part in parts[:-1]:
            owner = getattr(owner, part)
        setattr(owner, parts[-1], AsyncMock(return_value=provider_events()))
        client.close = AsyncMock()
        holder = {}
        with (
            patch.object(module, client_factory, new=AsyncMock(return_value=client)),
            patch.object(module, request_builder, return_value={}),
        ):
            model = Mock(modelName="synthetic", provider=Mock(template_id="openai"))
            frames = [frame async for frame in _model_stream_response(stream_function(model, {}, holder, encode_model_stream_event))]
        client.close.assert_awaited_once()
        self.assertNotIn("result", holder)
        return [json.loads(frame.decode().split("data: ", 1)[1]) for frame in frames]

    async def test_anthropic_sdk_transport_failure_after_partial_output_is_a_typed_terminal_frame(self):
        request = httpx.Request("POST", "https://synthetic.invalid/messages")
        for error, reason in (
            (anthropic.APITimeoutError(request), "provider_timeout"),
            (anthropic.APIConnectionError(request=request), "provider_unreachable"),
        ):
            with self.subTest(error_type=type(error).__name__):
                frames = await self.frames_after_partial_output(anthropic_messages, error)
                self.assertEqual(frames, [
                    {"schema": "api.model.stream.v1", "type": "delta", "delta": "partial"},
                    {"schema": "api.model.stream.v1", "type": "error", "reasonType": reason, "httpStatus": None},
                ])

    async def test_all_adapters_preserve_raw_transport_and_http_failure_facts_after_partial_output(self):
        response = httpx.Response(403, request=httpx.Request("POST", "https://synthetic.invalid"))
        for module in (openai_completions, openai_responses, anthropic_messages):
            status_error = anthropic.APIStatusError if module is anthropic_messages else APIStatusError
            for error, reason, status in (
                (httpx.ReadTimeout("private endpoint"), "provider_timeout", None),
                (httpx.ReadError("private endpoint"), "provider_unreachable", None),
                (httpx.RemoteProtocolError("private response"), "provider_stream_interrupted", None),
                (status_error("private body", response=response, body=None), "provider_authentication_failed", 403),
            ):
                with self.subTest(adapter=module.__name__, error_type=type(error).__name__):
                    frames = await self.frames_after_partial_output(module, error)
                    self.assertEqual(len(frames), 2)
                    self.assertEqual(frames[0]["delta"], "partial")
                    self.assertEqual(frames[1], {
                        "schema": "api.model.stream.v1", "type": "error", "reasonType": reason, "httpStatus": status,
                    })
                    self.assertNotIn("private", json.dumps(frames))
