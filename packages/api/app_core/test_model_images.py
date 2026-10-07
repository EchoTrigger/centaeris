"""Image admission and provider wire content, without remote model calls."""

import base64
import copy
import json
from io import BytesIO
from types import SimpleNamespace

from django.test import SimpleTestCase, TransactionTestCase
from django.conf import settings
from django.contrib.auth import get_user_model
from unittest.mock import patch
from PIL import Image

from .model_adapter.common import ModelProviderError, validate_prepared_prompt
from .model_adapter.openai_completions import build_open_ai_completions_request
from .model_adapter.openai_responses import build_open_ai_responses_request
from .model_adapter.anthropic_messages import build_anthropic_messages_request
from .agent_run_authorization_factory import create_agent_run_authorization
from .models import Workspace, ModelConfig, AgentRun
from .testing import create_session
from .runtime_contract import MODEL_RUN_SCHEMA
from .core_test_fixtures import core_fixture


def image_prompt():
    stream = BytesIO()
    Image.new("RGB", (2, 3)).save(stream, format="PNG")
    data = base64.b64encode(stream.getvalue()).decode("ascii")
    prompt = {
        "schema": "prepared_prompt.v1", "systemPrompt": "system", "messages": [
            {"messageId": "u1", "role": "user", "content": "before [one] middle [two] after"},
            {"messageId": "a1", "role": "assistant", "content": "text answer"},
        ], "toolDefinitions": [], "toolChoice": {"type": "none"}, "maxOutputTokens": 64,
        "inputImages": [{"messageId": "u1", "contentType": "image/png", "placeholder": placeholder, "dataBase64": data} for placeholder in ["[two]", "[one]"]],
    }
    return prompt


class ModelImageTests(SimpleTestCase):
    model = SimpleNamespace(maxOutputTokens=128, modelName="fixture", thinkingModes=[])

    def test_internal_body_limit_is_scoped_and_inclusive(self):
        from .http.internal_model import MODEL_RUN_MAX_BODY_BYTES
        self.assertEqual(MODEL_RUN_MAX_BODY_BYTES, 128 * 1024 * 1024)
        original_limit = settings.DATA_UPLOAD_MAX_MEMORY_SIZE
        with patch("app_core.http.internal_model.MODEL_RUN_MAX_BODY_BYTES", 1024):
            for size, status, error in [(1024, 400, "schema_mismatch"), (1025, 413, "model_run_request_too_large")]:
                with self.subTest(size=size):
                    response = self.client.post("/internal/model-runs", data=b"{}" + b" " * (size - 2), content_type="application/json", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
                    self.assertEqual(response.status_code, status)
                    self.assertEqual(response.json()["error"], error)
        self.assertEqual(settings.DATA_UPLOAD_MAX_MEMORY_SIZE, original_limit)

    def test_providers_send_inline_images_in_text_order(self):
        prompt = image_prompt()
        original = copy.deepcopy(prompt)
        data = prompt["inputImages"][0]["dataBase64"]
        body = {"preparedPrompt": prompt}
        openai = build_open_ai_completions_request(self.model, body)["messages"][1]["content"]
        responses = build_open_ai_responses_request(self.model, body)["input"][0]["content"]
        anthropic = build_anthropic_messages_request(self.model, body)["messages"][0]["content"]
        for blocks, text_type, image_type in [(openai, "text", "image_url"), (responses, "input_text", "input_image"), (anthropic, "text", "image")]:
            self.assertEqual([part["type"] for part in blocks], [text_type, image_type, text_type, image_type, text_type])
            self.assertEqual([blocks[i]["text"] for i in [0, 2, 4]], ["before ", " middle ", " after"])
        self.assertEqual(openai[1]["image_url"]["url"], "data:image/png;base64," + data)
        self.assertEqual(responses[1]["image_url"], "data:image/png;base64," + data)
        self.assertEqual(anthropic[1]["source"], {"type": "base64", "media_type": "image/png", "data": data})
        self.assertEqual(body["preparedPrompt"], original)

    def test_core_generated_corpus_and_limits(self):
        from .model_adapter.images import MODEL_INPUT_IMAGE_MAX_BYTES, MODEL_INPUT_IMAGE_MAX_PIXELS
        path = core_fixture("model_images.json")
        corpus = json.loads(path.read_text(encoding="utf-8"))
        self.assertEqual(corpus["maxImageBytes"], MODEL_INPUT_IMAGE_MAX_BYTES)
        self.assertEqual(corpus["maxImagePixels"], MODEL_INPUT_IMAGE_MAX_PIXELS)
        for case in corpus["cases"]:
            with self.subTest(content_type=case["contentType"]):
                body = {"preparedPrompt": case["preparedPrompt"]}
                validate_prepared_prompt(self.model, body)
                chat = build_open_ai_completions_request(self.model, body)["messages"][1]["content"]
                responses = build_open_ai_responses_request(self.model, body)["input"][0]["content"]
                anthropic = build_anthropic_messages_request(self.model, body)["messages"][0]["content"]
                encoded = case["preparedPrompt"]["inputImages"][0]["dataBase64"]
                url = "data:" + case["contentType"] + ";base64," + encoded
                self.assertEqual(chat[1]["image_url"]["url"], url)
                self.assertEqual(responses[1]["image_url"], url)
                self.assertEqual(anthropic[1]["source"]["data"], encoded)

    def test_empty_messages_do_not_shift_image_target(self):
        prompt = image_prompt()
        prompt["messages"].append({"messageId": "empty", "role": "user", "content": " "})
        messages = build_open_ai_completions_request(self.model, {"preparedPrompt": prompt})["messages"]
        self.assertEqual(messages[0], {"role": "system", "content": "system"})
        self.assertIsInstance(messages[1]["content"], list)
        self.assertEqual(messages[2]["content"], "text answer")

    def test_image_bound_to_missing_content_is_contract_error(self):
        prompt = image_prompt()
        del prompt["messages"][0]["content"]
        with self.assertRaisesRegex(ModelProviderError, "^prepared_prompt_image_invalid$"):
            validate_prepared_prompt(self.model, {"preparedPrompt": prompt})

    def test_invalid_images_fail_before_provider_projection(self):
        cases = [
            ({"dataBase64": "not base64"}, "prepared_prompt_image_base64_invalid"),
            ({"dataBase64": base64.b64encode(b"not an image").decode()}, "prepared_prompt_image_content_invalid"),
            ({"contentType": "image/jpeg"}, "prepared_prompt_image_content_type_mismatch"),
            ({"messageId": "missing"}, "prepared_prompt_image_message_missing"),
            ({"messageId": "a1"}, "prepared_prompt_image_invalid"),
            ({"placeholder": "absent"}, "prepared_prompt_image_invalid"),
            ({"unknown": True}, "prepared_prompt_image_invalid"),
        ]
        for changes, expected in cases:
            prompt = image_prompt()
            prompt["inputImages"][0].update(changes)
            with self.subTest(changes=list(changes)), self.assertRaisesRegex(ModelProviderError, "^" + expected + "$"):
                validate_prepared_prompt(self.model, {"preparedPrompt": prompt})

    def test_ten_mib_boundary_and_duplicate_bindings(self):
        prompt = image_prompt()
        image = prompt["inputImages"][0]
        decoded = base64.b64decode(image["dataBase64"])
        decoded += b"\0" * (10 * 1024 * 1024 - len(decoded))
        image["dataBase64"] = base64.b64encode(decoded).decode()
        validate_prepared_prompt(self.model, {"preparedPrompt": prompt})
        image["dataBase64"] = base64.b64encode(decoded + b"\0").decode()
        with self.assertRaisesRegex(ModelProviderError, "model_input_image_byte_length_invalid"):
            validate_prepared_prompt(self.model, {"preparedPrompt": prompt})
        prompt = image_prompt()
        prompt["inputImages"].append(copy.deepcopy(prompt["inputImages"][0]))
        with self.assertRaisesRegex(ModelProviderError, "prepared_prompt_image_invalid"):
            validate_prepared_prompt(self.model, {"preparedPrompt": prompt})


class InternalModelImageTests(TransactionTestCase):
    serialized_rollback = True

    def test_core_image_crosses_http_validation_and_reaches_provider(self):
        user = get_user_model().objects.create_user(username="image-contract", password="password")
        workspace = Workspace.objects.create(name="Images", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(id="image-contract", displayName="Images", maxOutputTokens=64)
        session = create_session(workspace=workspace, owner=user)
        run = AgentRun.objects.create(workspace=workspace, session=session, user=user, modelConfig=model, prompt="image")
        authorization = create_agent_run_authorization(run, image_digest="sha256:" + "a" * 64)
        path = core_fixture("model_images.json")
        prompt = json.loads(path.read_text(encoding="utf-8"))["cases"][0]["preparedPrompt"]
        decoded = base64.b64decode(prompt["inputImages"][0]["dataBase64"])
        prompt["inputImages"][0]["dataBase64"] = base64.b64encode(
            decoded + b"\0" * (10 * 1024 * 1024 - len(decoded))
        ).decode("ascii")
        body = {"schema": MODEL_RUN_SCHEMA, "agentRunId": run.id, "modelConfigRef": model.id,
                "maxOutputTokens": 64, "authorizationRef": authorization.id,
                "authorizationDigest": authorization.digest, "preparedPrompt": prompt}

        def provider(**kwargs):
            projected = build_open_ai_completions_request(model, kwargs["request_body"])
            self.assertEqual(projected["messages"][1]["content"][1]["type"], "image_url")
            return {"ok": True}

        with patch("app_core.http.internal_model.run_model", side_effect=provider) as invoked:
            def submit():
                return self.client.post("/internal/model-runs", data=json.dumps(body), content_type="application/json", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
            response = submit()
            self.assertEqual(response.status_code, 200, response.content)
            invoked.assert_called_once()
            prompt["inputImages"][0]["dataBase64"] = "invalid"
            response = submit()
            self.assertEqual(response.status_code, 400, response.content)
            self.assertEqual(response.json()["error"], "prepared_prompt_image_base64_invalid")
            invoked.assert_called_once()
