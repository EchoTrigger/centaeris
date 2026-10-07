"""Agent conversation delivery guidance at the authorized provider boundary."""

import copy
import json
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock, patch

from asgiref.sync import async_to_sync
from django.conf import settings
from django.contrib.auth import get_user_model
from django.test import SimpleTestCase, TransactionTestCase

from .agent_messages import _CONTRACT, message_contract_digest
from .agent_run_authorization_factory import create_agent_run_authorization
from .credentials import encrypt_credential_secret
from .model_adapter.anthropic_messages import build_anthropic_messages_request
from .model_adapter.openai_completions import build_open_ai_completions_request
from .model_adapter.openai_responses import build_open_ai_responses_request
from .models import (
    Agent,
    AgentCoordinationSession,
    AgentRun,
    ModelConfig,
    ModelProvider,
    ModelQuotaDomain,
    ProviderCredential,
    Session,
    SessionEvent,
    Workspace,
    WorkspaceMembership,
)
from .runtime_contract import MODEL_RUN_SCHEMA


PROVIDERS = (
    ("openai-completions", "openai_completions", "open_ai_completions", build_open_ai_completions_request),
    ("openai-responses", "openai_responses", "open_ai_responses", build_open_ai_responses_request),
    ("anthropic-messages", "anthropic_messages", "anthropic_messages", build_anthropic_messages_request),
)
REPLY = {"body": "你好！有什么我可以帮你的吗？", "session_refs": [], "file_refs": []}


class ConversationRoleGuidanceTests(SimpleTestCase):
    def prompt(self, images):
        from .agent_message_prompt import conversation_provider_prompt
        run = SimpleNamespace(session_id="conversation", session=SimpleNamespace(agent_id="agent"))
        body = {"systemPrompt": "Base", "toolChoice": {"type": "auto"},
                "toolDefinitions": [message_definition()], "inputImages": images}
        original = copy.deepcopy(body)
        with patch("app_core.agent_message_prompt.AgentWorkSession.objects") as work, \
                patch("app_core.agent_message_prompt.AgentCoordinationSession.objects") as coordinator:
            work.select_related.return_value.filter.return_value.first.return_value = None
            coordinator.filter.return_value.exists.return_value = True
            result = conversation_provider_prompt(run, body)
        self.assertEqual(body, original)
        self.assertEqual(result["toolDefinitions"], body["toolDefinitions"])
        return result["systemPrompt"]

    def test_coordinator_answers_simple_questions_before_dispatching_work(self):
        system = self.prompt([])
        self.assertIn("Answer simple questions directly", system)
        self.assertIn("send_message", system)
        self.assertNotIn("native image", system)

    def test_visible_image_guidance_does_not_imply_a_local_file(self):
        system = self.prompt([{"messageId": "image"}])
        self.assertIn("native image", system)
        self.assertIn("OCR", system)
        self.assertIn("Do not guess local paths", system)
        self.assertLess(len(system) - len("Base"), 1024)


def message_definition():
    return {"name": _CONTRACT["name"], "description": _CONTRACT["summary"],
            "inputSchema": copy.deepcopy(_CONTRACT["inputSchema"])}


def native_system(api, payload):
    if api == "openai-completions":
        return "\n\n".join(message["content"] for message in payload["messages"] if message["role"] == "system")
    return payload.get("instructions" if api == "openai-responses" else "system", "")


def provider_response(api):
    if api == "openai-completions":
        return {"choices": [{"message": {"content": "", "tool_calls": [{"id": "greeting", "type": "function",
            "function": {"name": "send_message", "arguments": json.dumps(REPLY)}}]}}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 10, "total_tokens": 20}}
    if api == "openai-responses":
        return {"output": [{"type": "function_call", "call_id": "greeting", "name": "send_message",
            "arguments": json.dumps(REPLY)}], "usage": {"input_tokens": 10, "output_tokens": 10, "total_tokens": 20}}
    return {"content": [{"type": "tool_use", "id": "greeting", "name": "send_message", "input": REPLY}],
            "usage": {"input_tokens": 10, "output_tokens": 10}}


def provider_events(api):
    if api == "openai-completions":
        response = provider_response(api)
        return [{"choices": [{"delta": {"tool_calls": [{"index": 0, **response["choices"][0]["message"]["tool_calls"][0]}]},
            "finish_reason": "tool_calls"}], "usage": response["usage"]}]
    if api == "openai-responses":
        return [{"type": "response.completed", "response": provider_response(api)}]
    return [
        {"type": "message_start", "message": {"usage": {"input_tokens": 10, "output_tokens": 0}}},
        {"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "greeting",
            "name": "send_message", "input": REPLY}},
        {"type": "message_delta", "usage": {"output_tokens": 10}},
        {"type": "message_stop"},
    ]


def client_for(api, create, close):
    resource = SimpleNamespace(create=create)
    if api == "openai-completions":
        return SimpleNamespace(chat=SimpleNamespace(completions=resource), close=close)
    return SimpleNamespace(**{"responses" if api == "openai-responses" else "messages": resource}, close=close)


class AgentMessagePromptTests(TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        self.user = get_user_model().objects.create_user(username="agent-greeting-owner")
        self.workspace = Workspace.objects.create(name="Agent greeting", createdBy=self.user)
        self.membership = WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        self.quota = ModelQuotaDomain.objects.create(id="greeting-quota", maxConcurrent=4, enabled=True)
        self.client.force_login(self.user)

    def run_for(self, api, *, bound=True):
        provider = ModelProvider.objects.create(displayName=api, api=api, apiBase="https://provider.example.invalid/v1")
        ProviderCredential.objects.create(provider=provider, quotaDomain=self.quota, displayName="Synthetic credential",
            encryptedSecret=encrypt_credential_secret("synthetic-key"), createdBy=self.user, updatedBy=self.user)
        model = ModelConfig.objects.create(provider=provider, displayName=api, modelName="synthetic-greeting", maxOutputTokens=64)
        agent = Agent.objects.create(workspace=self.workspace, owner=self.user, name="Greeting Agent", instructions="x" * 16000)
        coordination = Session.objects.create(workspace=self.workspace, owner=self.user, agent=agent)
        AgentCoordinationSession.objects.create(agent=agent, session=coordination)
        session = coordination if bound else Session.objects.create(workspace=self.workspace, owner=self.user, agent=agent)
        run = AgentRun.objects.create(workspace=self.workspace, session=session, user=self.user, modelConfig=model,
            prompt="你好", agent_instructions=agent.instructions, status="running")
        authorization = create_agent_run_authorization(run, image_digest="sha256:" + "a" * 64)
        body = {"schema": MODEL_RUN_SCHEMA, "agentRunId": run.id, "modelConfigRef": model.id,
            "authorizationRef": authorization.id, "authorizationDigest": authorization.digest, "maxOutputTokens": 64,
            "preparedPrompt": {"schema": "prepared_prompt.v1", "systemPrompt": "Original Agent system",
                "messages": [{"messageId": "user-greeting", "role": "user", "content": "你好"}],
                "toolDefinitions": [message_definition()], "toolChoice": {"type": "auto"}, "maxOutputTokens": 64}}
        return run, body

    def submit(self, body, *, stream=False, token=None):
        return self.client.post("/internal/model-runs", data=json.dumps(body), content_type="application/json",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN if token is None else token,
            **({"HTTP_ACCEPT": "text/event-stream"} if stream else {}))

    def assert_guidance(self, api, payload, body):
        system = native_system(api, payload)
        self.assertTrue(system.startswith("Original Agent system\n\n"))
        for direction in ("send_message", "greetings", "Final"):
            self.assertIn(direction, system)
        self.assertLess(len(system) - len("Original Agent system"), 1024)
        baseline = dict(body)
        baseline["preparedPrompt"] = {**body["preparedPrompt"], "systemPrompt": system}
        builder = next(provider[3] for provider in PROVIDERS if provider[0] == api)
        expected = builder(ModelConfig.objects.get(id=body["modelConfigRef"]), baseline)
        if "stream" in payload:
            expected["stream"] = True
            if api == "openai-completions":
                expected["stream_options"] = {"include_usage": True}
        self.assertEqual(payload, expected)

    def assert_source_facts_unchanged(self, run, body, original, authorization_payload, digest):
        self.assertEqual(body, original)
        run.refresh_from_db()
        run.session.agent.refresh_from_db()
        run.authorization.refresh_from_db()
        self.assertEqual(run.prompt, "你好")
        self.assertEqual(run.agent_instructions, "x" * 16000)
        self.assertEqual(run.session.agent.instructions, "x" * 16000)
        self.assertEqual(run.authorization.payload, authorization_payload)
        self.assertEqual(message_contract_digest(), digest)
        self.assertFalse(SessionEvent.objects.filter(session=run.session).exists())

    def test_natural_greeting_reaches_each_provider_with_native_delivery_guidance(self):
        for api, module, client_name, _ in PROVIDERS:
            with self.subTest(api=api):
                run, body = self.run_for(api)
                original, authorization = copy.deepcopy(body), copy.deepcopy(run.authorization.payload)
                digest = message_contract_digest()
                create = Mock(return_value=provider_response(api))
                close = Mock()
                with patch(f"app_core.model_adapter.{module}.{client_name}_client", return_value=client_for(api, create, close)):
                    response = self.submit(body)
                self.assertEqual(response.status_code, 200, response.content)
                create.assert_called_once()
                close.assert_called_once()
                self.assert_guidance(api, create.call_args.kwargs, body)
                self.assertEqual(response.json()["toolCalls"][0]["name"], "send_message")
                self.assertEqual(json.loads(response.json()["toolCalls"][0]["argsJson"]), REPLY)
                self.assert_source_facts_unchanged(run, body, original, authorization, digest)

    def test_streaming_greeting_has_the_same_native_guidance_and_no_publication_side_effect(self):
        async def events(api):
            for event in provider_events(api):
                yield event

        async def consume(response):
            return b"".join([chunk async for chunk in response.streaming_content])

        for api, module, client_name, _ in PROVIDERS:
            with self.subTest(api=api):
                run, body = self.run_for(api)
                original, authorization = copy.deepcopy(body), copy.deepcopy(run.authorization.payload)
                digest = message_contract_digest()
                create, close = AsyncMock(return_value=events(api)), AsyncMock()
                client = client_for(api, create, close)
                with patch(f"app_core.model_adapter.{module}.async_{client_name}_client", new=AsyncMock(return_value=client)):
                    response = self.submit(body, stream=True)
                    self.assertEqual(response.status_code, 200)
                    output = async_to_sync(consume)(response).decode("utf-8")
                create.assert_awaited_once()
                close.assert_awaited_once()
                self.assert_guidance(api, create.call_args.kwargs, body)
                self.assertNotIn("event: error", output)
                self.assertEqual(output.count("event: result"), 1)
                terminal = json.loads(output.split("data: ", 1)[1])
                self.assertEqual(terminal["toolCalls"][0]["name"], "send_message")
                self.assertEqual(json.loads(terminal["toolCalls"][0]["argsJson"]), REPLY)
                self.assert_source_facts_unchanged(run, body, original, authorization, digest)

    def test_ordinary_work_session_preserves_each_provider_request(self):
        for api, module, client_name, builder in PROVIDERS:
            with self.subTest(api=api):
                run, body = self.run_for(api, bound=False)
                expected = builder(run.modelConfig, body)
                create = Mock(return_value=provider_response(api))
                with patch(f"app_core.model_adapter.{module}.{client_name}_client", return_value=client_for(api, create, Mock())):
                    response = self.submit(body)
                self.assertEqual(response.status_code, 200, response.content)
                self.assertEqual(create.call_args.kwargs, expected)

    def test_required_and_specific_message_choices_keep_their_provider_mapping(self):
        for api, module, client_name, _ in PROVIDERS:
            run, body = self.run_for(api)
            for choice in ({"type": "required"}, {"type": "specific", "name": "send_message"}):
                with self.subTest(api=api, choice=choice):
                    body["preparedPrompt"]["toolChoice"] = choice
                    create = Mock(return_value=provider_response(api))
                    with patch(f"app_core.model_adapter.{module}.{client_name}_client", return_value=client_for(api, create, Mock())):
                        response = self.submit(body)
                    self.assertEqual(response.status_code, 200, response.content)
                    self.assert_guidance(api, create.call_args.kwargs, body)

    def test_optional_system_and_repeated_requests_do_not_rewrite_or_accumulate_prompt_facts(self):
        for api, module, client_name, builder in PROVIDERS:
            run, body = self.run_for(api)
            for present in (False, True):
                with self.subTest(api=api, explicit_null_system=present):
                    body["preparedPrompt"].pop("systemPrompt", None)
                    if present:
                        body["preparedPrompt"]["systemPrompt"] = None
                    original = copy.deepcopy(body)
                    create = Mock(return_value=provider_response(api))
                    with patch(f"app_core.model_adapter.{module}.{client_name}_client", return_value=client_for(api, create, Mock())):
                        for _ in range(2):
                            response = self.submit(body)
                            self.assertEqual(response.status_code, 200, response.content)
                    first, second = [call.kwargs for call in create.call_args_list]
                    self.assertEqual(first, second)
                    system = native_system(api, first)
                    self.assertIn("greetings", system)
                    self.assertIn("send_message", system)
                    self.assertLess(len(system), 1024)
                    expected = builder(run.modelConfig, {**body, "preparedPrompt": {**body["preparedPrompt"], "systemPrompt": system}})
                    self.assertEqual(first, expected)
                    self.assertEqual(body, original)

    def test_auxiliary_and_noncanonical_requests_remain_unchanged(self):
        other = {"name": "read", "description": "Read", "inputSchema": {"type": "object"}}
        changed_description = {**message_definition(), "description": "A different tool"}
        changed_schema = {**message_definition(), "inputSchema": {"type": "object"}}
        cases = (
            ("compaction", [], {"type": "none"}),
            ("disabled messaging", [message_definition()], {"type": "none"}),
            ("missing messaging", [other], {"type": "auto"}),
            ("other specific tool", [message_definition(), other], {"type": "specific", "name": "read"}),
            ("changed description", [changed_description], {"type": "auto"}),
            ("changed schema", [changed_schema], {"type": "auto"}),
        )
        for api, module, client_name, builder in PROVIDERS:
            run, body = self.run_for(api)
            for name, definitions, choice in cases:
                with self.subTest(api=api, case=name):
                    body["preparedPrompt"]["toolDefinitions"] = definitions
                    body["preparedPrompt"]["toolChoice"] = choice
                    expected = builder(run.modelConfig, body)
                    create = Mock(return_value=provider_response(api))
                    with patch(f"app_core.model_adapter.{module}.{client_name}_client", return_value=client_for(api, create, Mock())):
                        response = self.submit(body)
                    self.assertEqual(response.status_code, 200, response.content)
                    self.assertEqual(create.call_args.kwargs, expected)

    def test_rejected_authority_never_reaches_provider(self):
        _, body = self.run_for("openai-completions")
        with patch("app_core.http.internal_model.run_model") as run_model, patch("app_core.http.internal_model.stream_model_async") as stream_model:
            for stream in (False, True):
                with self.subTest(stream=stream):
                    self.assertEqual(self.submit(body, stream=stream, token="wrong").status_code, 401)
                    rejected = {**body, "authorizationDigest": "sha256:" + "0" * 64}
                    self.assertEqual(self.submit(rejected, stream=stream).status_code, 404)
            self.membership.delete()
            for stream in (False, True):
                self.assertEqual(self.submit(body, stream=stream).status_code, 404)
            run_model.assert_not_called()
            stream_model.assert_not_called()
