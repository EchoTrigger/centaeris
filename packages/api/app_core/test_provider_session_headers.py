from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock, patch

from django.test import SimpleTestCase

from .model_adapter import anthropic_messages, common, openai_completions, openai_responses

CLIENTS = (
    (openai_completions, "open_ai_completions", "OpenAI", "AsyncOpenAI"),
    (openai_responses, "open_ai_responses", "OpenAI", "AsyncOpenAI"),
    (anthropic_messages, "anthropic_messages", "Anthropic", "AsyncAnthropic"),
)


class ProviderSessionHeaderTests(SimpleTestCase):
    def model(self, template="opencode_zen_go"):
        return SimpleNamespace(id="model", provider=SimpleNamespace(template_id=template))

    def test_go_model_test_uses_a_stable_identity_and_product_user_agent(self):
        for module, name, sdk, _ in CLIENTS:
            with self.subTest(protocol=name):
                clients = []
                for _ in range(2):
                    with (
                        patch.object(module, "resolve_model_route", return_value=(name, "https://provider.invalid/v1")),
                        patch.object(module, "resolve_model_secret", return_value="test-secret"),
                        patch.object(module, sdk, new=Mock()) as create,
                    ):
                        getattr(module, f"{name}_client")(self.model())
                        clients.append(create.call_args.kwargs)
                headers = clients[0]["default_headers"]
                self.assertEqual(headers["x-opencode-session"], "model-test:model")
                self.assertEqual(headers["User-Agent"], "CentaerisWorkspace")
                self.assertEqual(clients[1]["default_headers"], headers)

    async def test_go_runtime_stream_uses_the_authoritative_session_for_its_run(self):
        # HTTP validates the Run authorization first; the adapter resolves the
        # retained hosted identity rather than accepting a caller-supplied header.
        for module, name, _, sdk in CLIENTS:
            with (
                self.subTest(protocol=name),
                patch.object(module, "resolve_model_route", return_value=(name, "https://provider.invalid/v1")),
                patch.object(module, "resolve_model_secret", return_value="test-secret"),
                patch.object(common.AgentRun.objects, "get", return_value=SimpleNamespace(session_id="session-owned")) as get,
                patch.object(module, sdk, new=Mock()) as create,
            ):
                await getattr(module, f"async_{name}_client")(self.model(), {"agentRunId": "run", "sessionId": "forged"})
                get.assert_called_once_with(id="run", modelConfig_id="model")
                self.assertEqual(create.call_args.kwargs["default_headers"]["x-opencode-session"], "session-owned")

    def test_unrelated_provider_keeps_its_client_headers(self):
        for module, name, sdk, _ in CLIENTS:
            with (
                self.subTest(protocol=name),
                patch.object(module, "resolve_model_route", return_value=(name, "https://provider.invalid/v1")),
                patch.object(module, "resolve_model_secret", return_value="test-secret"),
                patch.object(module, sdk, new=Mock()) as create,
            ):
                getattr(module, f"{name}_client")(self.model("other"))
                self.assertNotIn("default_headers", create.call_args.kwargs)

    def test_new_runs_in_the_same_session_keep_the_routing_identity(self):
        with patch.object(common.AgentRun.objects, "get", return_value=SimpleNamespace(session_id="session-owned")):
            headers = [common.provider_client_options(self.model(), {"agentRunId": run}) for run in ("run-1", "run-2")]
        self.assertEqual(headers[0], headers[1])
        self.assertEqual(headers[0]["default_headers"]["x-opencode-session"], "session-owned")

    def test_missing_authoritative_run_does_not_use_the_test_identity(self):
        with patch.object(common.AgentRun.objects, "get", side_effect=common.AgentRun.DoesNotExist):
            with self.assertRaisesMessage(common.ModelProviderError, "model_session_unavailable"):
                common.provider_client_options(self.model(), {"agentRunId": "missing"})

    async def test_stream_requests_carry_the_run_identity_to_the_sdk(self):
        async def chunks():
            yield {"choices": [{"delta": {"content": "OK"}, "finish_reason": "stop"}],
                   "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}

        model = self.model()
        body = {"agentRunId": "run"}
        client = Mock()
        client.chat.completions.create = AsyncMock(return_value=chunks())
        client.close = AsyncMock()
        with (
            patch.object(openai_completions, "resolve_model_route", return_value=("openai-completions", "https://provider.invalid/v1")),
            patch.object(openai_completions, "resolve_model_secret", return_value="test-secret"),
            patch.object(common.AgentRun.objects, "get", return_value=SimpleNamespace(session_id="session-owned")),
            patch.object(openai_completions, "AsyncOpenAI", return_value=client) as create,
            patch.object(openai_completions, "build_open_ai_completions_request", return_value={}),
        ):
            holder = {}
            [frame async for frame in openai_completions.stream_open_ai_completions(model, body, holder, lambda *args: args)]
        self.assertEqual(holder["result"]["text"], "OK")
        self.assertEqual(create.call_args.kwargs["default_headers"]["x-opencode-session"], "session-owned")
