"""Hosted output truncation must reach Core without executing partial calls."""

import hashlib
import json
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock, patch

from django.test import SimpleTestCase

from .http import internal_model
from .http.internal_model import _model_stream_response
from .model_adapter import encode_model_stream_event
from .model_adapter import openai_completions
from .model_adapter.common import ModelProviderError


class ModelOutputRecoveryTests(SimpleTestCase):
    usage = {"prompt_tokens": 10, "completion_tokens": 64000, "total_tokens": 64010}

    def test_sync_output_limit_keeps_rejected_tool_identity_without_parsing_partial_json(self):
        args = '{"command":"private unfinished'
        for finish in ("length", "max_tokens"):
            with self.subTest(finish=finish):
                with self.assertRaises(ModelProviderError) as raised:
                    openai_completions.parse_open_ai_completions_response({
                        "choices": [{"finish_reason": finish, "message": {
                            "content": "partial answer", "tool_calls": [{"id": "original-call", "function": {
                                "name": "bash", "arguments": args,
                            }}],
                        }}], "usage": self.usage,
                    })
                self.assertEqual(raised.exception.reasonType, "incomplete_output_token_limit")
                self.assertEqual(raised.exception.truncatedToolCalls, [{
                    "callId": "original-call", "toolName": "bash", "argsBytes": len(args.encode()),
                    "argsSha256": "sha256:" + hashlib.sha256(args.encode()).hexdigest(),
                }])

    async def stream(self, frames):
        async def source():
            for frame in frames:
                yield frame

        client = Mock()
        client.chat.completions.create = AsyncMock(return_value=source())
        client.close = AsyncMock()
        holder = {}
        with (
            patch.object(openai_completions, "async_open_ai_completions_client", new=AsyncMock(return_value=client)),
            patch.object(openai_completions, "build_open_ai_completions_request", return_value={}),
        ):
            chunks = [part async for part in _model_stream_response(openai_completions.stream_open_ai_completions(
                Mock(provider=Mock(template_id="openai")), {}, holder, encode_model_stream_event,
            ))]
        client.close.assert_awaited_once()
        return [json.loads(part.decode().split("data: ", 1)[1]) for part in chunks], holder

    async def test_stream_output_limit_preserves_visible_prefix_and_safe_rejected_call_metadata(self):
        args = '{"command":"private unfinished'
        frames, holder = await self.stream([
            {"choices": [{"delta": {"content": "partial answer", "tool_calls": [{
                "index": 0, "id": "original-call", "function": {"name": "bash", "arguments": args},
            }]}}]},
            {"choices": [{"delta": {}, "finish_reason": "length"}], "usage": self.usage},
        ])
        self.assertEqual(frames[0]["delta"], "partial answer")
        self.assertEqual(frames[-1]["reasonType"], "incomplete_output_token_limit")
        self.assertEqual(frames[-1]["truncatedToolCalls"][0]["callId"], "original-call")
        self.assertNotIn("private unfinished", json.dumps(frames))
        self.assertNotIn("result", holder)

    async def test_reasoning_only_length_is_output_limit_even_without_visible_body(self):
        frames, holder = await self.stream([
            {"choices": [{"delta": {"reasoning_content": "thinking"}}]},
            {"choices": [{"delta": {}, "finish_reason": "max_tokens"}], "usage": self.usage},
        ])
        self.assertEqual(frames[-1]["reasonType"], "incomplete_output_token_limit")
        self.assertEqual(frames[-1]["truncatedToolCalls"], [])
        self.assertNotIn("result", holder)

    def test_empty_stop_has_a_separate_retryable_identity(self):
        with self.assertRaises(ModelProviderError) as raised:
            openai_completions.parse_open_ai_completions_response({
                "choices": [{"finish_reason": "stop", "message": {"content": "", "reasoning_content": "thinking"}}],
                "usage": self.usage,
            })
        self.assertEqual(raised.exception.reasonType, "empty_final_response")

    async def test_stream_empty_stop_is_not_success(self):
        frames, holder = await self.stream([
            {"choices": [{"delta": {}, "finish_reason": "stop"}], "usage": self.usage},
        ])
        self.assertEqual(frames[-1]["reasonType"], "empty_final_response")
        self.assertNotIn("result", holder)

    def test_exact_token_usage_with_normal_finish_is_not_inferred_as_truncation(self):
        result = openai_completions.parse_open_ai_completions_response({
            "choices": [{"finish_reason": "stop", "message": {"content": "complete"}}], "usage": self.usage,
        })
        self.assertEqual(result["text"], "complete")

    async def test_nonstream_http_failure_carries_the_same_output_limit_identity(self):
        error = ModelProviderError("incomplete_output_token_limit", truncated_tool_calls=[])
        request = SimpleNamespace(read=lambda limit: b'{"agentRunId":"run"}', headers={})
        with (
            patch.object(internal_model, "_validate_model_run", new=AsyncMock(return_value="model")),
            patch.object(internal_model, "run_model", side_effect=error),
        ):
            response = await internal_model.model_runs(request)
        self.assertEqual(response.status_code, 502)
        self.assertEqual(json.loads(response.content), {
            "error": "model_run_failed", "reasonType": "incomplete_output_token_limit",
            "httpStatus": None, "truncatedToolCalls": [],
        })

    def test_truncation_with_missing_identity_retains_digest_and_never_becomes_a_tool_call(self):
        with self.assertRaises(ModelProviderError) as raised:
            openai_completions.parse_open_ai_completions_response({
                "choices": [{"finish_reason": "length", "message": {"tool_calls": [{
                    "function": {"arguments": "partial"},
                }]}}], "usage": self.usage,
            })
        self.assertEqual(raised.exception.truncatedToolCalls[0]["callId"], None)
        self.assertEqual(raised.exception.truncatedToolCalls[0]["toolName"], None)
        self.assertEqual(raised.exception.truncatedToolCalls[0]["argsBytes"], 7)
