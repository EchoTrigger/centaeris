"""Control characters survive hosted storage without changing tool outcomes."""
import json
import unittest
import importlib
from pathlib import Path

from .session_payload import decode_session_payload, encode_session_payload
from django.db import connection
from django.test import TransactionTestCase
from . import test_agent_messages as fixture
from .models import SessionEvent


def strings(value):
    if isinstance(value, str):
        yield value
    elif isinstance(value, dict):
        for key, item in value.items():
            yield key
            yield from strings(item)
    elif isinstance(value, list):
        for item in value:
            yield from strings(item)


class SessionPayloadCodecTests(unittest.TestCase):
    def test_storage_contract_matches_the_shared_rust_python_corpus(self):
        corpus = Path(__file__).resolve().parents[3] / "tests/workspace/fixtures/session_payload_storage.json"
        cases = json.loads(corpus.read_text(encoding="utf-8"))
        self.assertGreaterEqual(len(cases), 3)
        for case in cases:
            with self.subTest(case=case["name"]):
                self.assertEqual(encode_session_payload(case["wire"]), case["stored"])
                self.assertEqual(decode_session_payload(case["stored"]), case["wire"])

    def test_tool_result_round_trip_retains_nul_and_literal_escape_separately(self):
        wire = {"schemaVersion": "session.event.v1", "type": "tool_result", "payload": {
            "callId": "call_fixture", "toolName": "bash", "resultState": "successWithOutput",
            "modelContent": "kernel\0\nLiteral \\u0000\n中文", "summary": "kernel\0",
        }}
        stored = encode_session_payload(wire)
        self.assertTrue(all("\0" not in item for item in strings(stored)))
        self.assertEqual(stored["type"], "tool_result")
        self.assertEqual(stored["payload"]["resultState"], "successWithOutput")
        self.assertEqual(decode_session_payload(json.loads(json.dumps(stored))), wire)

    def test_normal_wire_is_unchanged(self):
        wire = {"type": "tool_result", "payload": {"modelContent": "Literal \\u0000", "resultState": "error"}}
        self.assertEqual(encode_session_payload(wire), wire)
        self.assertEqual(decode_session_payload(wire), wire)

    def test_query_index_cannot_be_tampered_independently_of_canonical_wire(self):
        stored = encode_session_payload({"type": "tool_result", "payload": {"callId": "owned", "modelContent": "a\0b"}})
        stored["payload"]["callId"] = "forged"
        with self.assertRaisesRegex(ValueError, "session_payload_storage_index_mismatch"):
            decode_session_payload(stored)

    def test_unknown_storage_schema_fails_closed(self):
        with self.assertRaisesRegex(ValueError, "session_payload_storage_invalid"):
            decode_session_payload({"__centaerisSessionStorage": {"schema": "unknown", "canonicalJson": "{}"}})

    def test_storage_field_remains_a_json_field_for_existing_indexes(self):
        from .session_payload import SessionPayloadField
        from django.db import models
        self.assertTrue(issubclass(SessionPayloadField, models.JSONField))


class HostedSessionPayloadTests(TransactionTestCase):
    serialized_rollback = True
    setUp = fixture.AgentMessageTests.setUp
    bind = fixture.AgentMessageTests.bind
    message_run = fixture.AgentMessageTests.message_run
    commit = fixture.AgentMessageTests.commit

    def test_tool_output_readback_is_lossless_through_instances_values_and_query_indexes(self):
        source = self.bind()
        run = self.message_run(source)
        self.commit(run, "tool_result", {"toolName": "bash", "resultState": "successWithOutput",
            "modelContent": "kernel\0 and literal \\u0000", "summary": "kernel\0"})
        event = SessionEvent.objects.get(agent_run=run)
        self.assertEqual(event.payload["payload"]["modelContent"], "kernel\0 and literal \\u0000")
        selected = SessionEvent.objects.filter(payload__type="tool_result",
            payload__payload__toolName="bash").values("payload").get()
        self.assertEqual(selected["payload"], event.payload)
        with connection.cursor() as cursor:
            cursor.execute('SELECT payload FROM app_core_sessionevent WHERE "eventId"=%s', [event.pk])
            stored = cursor.fetchone()[0]
        if isinstance(stored, str):
            stored = json.loads(stored)
        self.assertTrue(all("\0" not in item for item in strings(stored)))
        self.assertEqual(decode_session_payload(stored), event.payload)

    def test_migration_refuses_to_remove_the_decoder_after_control_byte_output(self):
        from django.apps import apps
        from types import SimpleNamespace
        source = self.bind()
        run = self.message_run(source)
        self.commit(run, "tool_result", {"toolName": "bash", "resultState": "failed", "modelContent": "failure\0detail"})
        migration = importlib.import_module("app_core.migrations.0006_session_event_payload_storage")
        with self.assertRaisesRegex(ValueError, "session_payload_migration_cannot_discard_canonical_output"):
            migration.refuse_lossy_reverse(apps, SimpleNamespace(connection=connection))
