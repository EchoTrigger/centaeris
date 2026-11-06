"""The external v1 OpenAPI must describe the same wire contract as Web's reference."""
import json
import re
from pathlib import Path

from django.test import SimpleTestCase
from pydantic.alias_generators import to_camel

from . import agent_run_api_protocol as wire


class AgentRunApiOpenApiTests(SimpleTestCase):
    @classmethod
    def setUpClass(cls):
        super().setUpClass()
        from api.ninja_api import api
        cls.schema = api.get_openapi_schema()
        reference_path = Path(__file__).resolve().parents[3] / "packages/web/src/developer/agentRunApiReference.json"
        cls.reference = json.loads(reference_path.read_text(encoding="utf-8"))
        cls.routes = {
            "createRun": ("post", "/api" + wire.CREATE_RUN_PATH),
            "sendMessage": ("post", "/api" + wire.SEND_MESSAGE_PATH),
            "closeConversation": ("post", "/api" + wire.CLOSE_CONVERSATION_PATH),
            "getOperation": ("get", "/api" + wire.GET_OPERATION_PATH),
            "cancelRun": ("post", "/api" + wire.CANCEL_RUN_PATH),
        }
        cls.operations = {name: cls.schema["paths"][path][method] for name, (method, path) in cls.routes.items()}

    def resolve_schema(self, value):
        if "$ref" in value:
            return self.schema["components"]["schemas"][value["$ref"].rsplit("/", 1)[-1]]
        return value

    def test_success_media_and_payloads_match_the_bundled_reference(self):
        for reference in self.reference["operations"]:
            with self.subTest(operation=reference["id"]):
                operation = self.operations[reference["id"]]
                method, path = self.routes[reference["id"]]
                self.assertEqual(method.upper(), reference["method"])
                public_path = re.sub(r"\{(\w+)\}", lambda match: "{" + to_camel(match[1]) + "}", path)
                self.assertEqual(public_path, reference["path"])
                responses = {str(code): response for code, response in operation["responses"].items()
                             if str(code).startswith("2")}
                expected_media = {}
                for response in reference["responses"]:
                    expected_media.setdefault(str(response["status"]), set()).add(response["contentType"])
                self.assertEqual(set(responses), set(expected_media))
                for status, media in expected_media.items():
                    self.assertEqual(set(responses[status].get("content", {})), media)
                for response in reference["responses"]:
                    media = responses[str(response["status"])]["content"][response["contentType"]]
                    if "schema" in response:
                        self.assertEqual(self.resolve_schema(media["schema"]), self.reference["schemas"][response["schema"]])
                    else:
                        self.assertEqual(media["schema"], {"type": "string"})

    def test_request_headers_and_bodies_match_the_bundled_reference(self):
        for reference in self.reference["operations"]:
            with self.subTest(operation=reference["id"]):
                operation = self.operations[reference["id"]]
                headers = {param["name"]: param for param in operation["parameters"] if param["in"] == "header"}
                expected_headers = {header["name"]: header for header in reference["headers"]
                                    if header["name"] != "Authorization"}
                self.assertEqual(set(headers), set(expected_headers))
                for name, expected in expected_headers.items():
                    self.assertEqual(headers[name]["required"], expected["required"])
                    self.assertEqual(headers[name]["schema"], expected["schema"])
                    self.assertTrue(headers[name]["description"])
                if "requestSchema" in reference:
                    body = operation["requestBody"]
                    self.assertIs(body["required"], True)
                    self.assertEqual(set(body["content"]), {"application/json"})
                    schema_ref = body["content"]["application/json"]["schema"]["$ref"]
                    self.assertEqual(self.schema["components"]["schemas"][schema_ref.rsplit("/", 1)[-1]],
                                     self.reference["schemas"][reference["requestSchema"]])
                else:
                    # In particular cancellation sends zero bytes, not a JSON object.
                    self.assertNotIn("requestBody", operation)
                self.assertTrue(operation["description"])

    def test_all_external_operations_require_only_the_agent_run_api_bearer(self):
        for operation in self.operations.values():
            self.assertEqual(len(operation["security"]), 1)
            requirement = operation["security"][0]
            self.assertEqual(len(requirement), 1)
            name, scopes = next(iter(requirement.items()))
            self.assertEqual(scopes, [])
            security = self.schema["components"]["securitySchemes"][name]
            self.assertEqual(security["type"], "http")
            self.assertEqual(security["scheme"], "bearer")
            self.assertTrue(security["description"])

    def test_retired_routes_return_not_found(self):
        for method, path in (
            ("POST", "/api/v1/agent-runs"),
            ("GET", "/api/v1/agent-runs/operations/operation"),
            ("POST", "/api/v1/agent-runs/run/cancel"),
        ):
            with self.subTest(method=method, path=path):
                self.assertEqual(self.client.generic(method, path).status_code, 404)
