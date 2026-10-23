from django.test import SimpleTestCase
from pydantic import ValidationError

from app_core.http.schema import ModelsEnvelope
from app_core.http.response_schema import AgentCoordinationSessionResponse
from app_core.http.workspaces import SessionMetadataRequest, SessionReadRequest


class SchemaWireContractTests(SimpleTestCase):
    def test_nested_response_roundtrip_preserves_exact_names_and_nulls(self):
        model = {
            "id": "model_1", "displayName": "Model", "providerId": None,
            "providerDisplayName": None, "modelName": "example", "contextTokens": 8192,
            "maxOutputTokens": 1024, "thinkingMode": None, "thinkingModes": [],
        }
        payload = {"models": [model]}
        self.assertEqual(ModelsEnvelope.model_validate(payload).model_dump(), payload)
        for old, new in [("displayName", "display_name"), ("modelName", "unknown")]:
            invalid = dict(model)
            invalid[new] = invalid.pop(old)
            with self.subTest(field=new), self.assertRaises(ValidationError):
                ModelsEnvelope.model_validate({"models": [invalid]})
        for invalid_tokens in [True, "8192", 8192.0]:
            with self.subTest(tokens=invalid_tokens), self.assertRaises(ValidationError):
                ModelsEnvelope.model_validate({"models": [model | {"contextTokens": invalid_tokens}]})

    def test_partial_request_preserves_unset_null_and_false(self):
        for payload in [{}, {"title": None}, {"isPinned": False}, {"isUnread": True}]:
            self.assertEqual(
                SessionMetadataRequest.model_validate(payload).model_dump(exclude_unset=True),
                payload,
            )
        for payload in [{"is_pinned": True}, {"isPinned": 1}, {"unknown": None}]:
            with self.subTest(payload=payload), self.assertRaises(ValidationError):
                SessionMetadataRequest.model_validate(payload)

    def test_observed_read_cursor_uses_canonical_lossless_decimal_strings(self):
        for value in ["0", "1", "9007199254740993", "9223372036854775807"]:
            self.assertEqual(SessionReadRequest.model_validate({"throughSequence": value}).model_dump(),
                             {"throughSequence": value})
        for payload in [{"throughSequence": value} for value in
                        [0, True, "01", "-1", "1.0", "1e0", " 1", "1\n", "9223372036854775808"]] + [
                            {"through_sequence": "1"}, {"throughSequence": "1", "unknown": True}]:
            with self.subTest(payload=payload), self.assertRaises(ValidationError):
                SessionReadRequest.model_validate(payload)

    def test_schema_literal_keeps_its_exceptional_wire_name(self):
        payload = {"schema": "agent.coordination_session.v1", "agentId": "agent_1", "sessionId": "session_1"}
        self.assertEqual(AgentCoordinationSessionResponse.model_validate(payload).model_dump(), payload)
        for field in ["schemaId", "schema_id"]:
            invalid = dict(payload)
            invalid[field] = invalid.pop("schema")
            with self.subTest(field=field), self.assertRaises(ValidationError):
                AgentCoordinationSessionResponse.model_validate(invalid)
