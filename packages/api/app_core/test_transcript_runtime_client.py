import io
import copy
from pathlib import Path
import json
import urllib.error
from unittest.mock import patch

from django.test import SimpleTestCase

from app_core.runtime_client import (
    TranscriptRuntimeError,
    request_transcript_page,
    request_transcript_patches,
    request_transcript_content,
)


def _http_conflict(payload: dict) -> urllib.error.HTTPError:
    return urllib.error.HTTPError(
        url="http://runtime/internal/transcript/page",
        code=409,
        msg="Conflict",
        hdrs=None,
        fp=io.BytesIO(json.dumps(payload).encode()),
    )


class TranscriptRuntimeClientTests(SimpleTestCase):
    def test_core_serialized_samples_pass_the_consumers_contract(self):
        from app_core.runtime_client import _valid_transcript_block
        samples = json.loads((Path(__file__).parent / "generated/transcript_block.samples.json").read_text(encoding="utf-8"))
        self.assertGreater(len(samples), 0)
        for sample in samples:
            self.assertTrue(_valid_transcript_block(sample), sample["body"]["kind"])

    def test_block_wire_names_null_fields_and_unknown_fields_are_strict(self):
        from app_core.runtime_client import _valid_transcript_block
        content = {"inlineContent": "hello", "sourceRef": None}
        bodies = [
            {"kind": "userText", "content": content},
            {"kind": "assistantText", "content": content, "status": "completed"},
            {"kind": "reasoning", "requestId": "r", "content": content, "status": "running"},
            {"kind": "tool", "callId": "c", "toolName": "read", "status": "completed",
             "summary": "read", "summaryRef": None, "outputRef": None},
            {"kind": "notice", "noticeType": "info", "content": content, "status": "completed"},
        ]
        for body in bodies:
            block = {"blockId": "b", "blockRevision": "1",
                     "orderKey": {"sourceSequence": "1", "ordinal": 0}, "body": body}
            self.assertTrue(_valid_transcript_block(block))
            for field in body:
                missing = copy.deepcopy(block)
                del missing["body"][field]
                self.assertFalse(_valid_transcript_block(missing), field)
            for path in [(), ("body",), ("orderKey",)]:
                extra = copy.deepcopy(block)
                obj = extra
                for key in path:
                    obj = obj[key]
                obj["unknown"] = "reject"
                self.assertFalse(_valid_transcript_block(extra))
            renamed = copy.deepcopy(block)
            renamed["block_id"] = renamed.pop("blockId")
            self.assertFalse(_valid_transcript_block(renamed))

    @patch("app_core.runtime_client.urllib.request.urlopen")
    def test_content_range_preserves_text_and_rejects_identity_or_continuation_drift(self, urlopen):
        request = {
            "schema": "transcript.content.range.read.v1", "sessionId": "session-1",
            "projectionVersion": "transcript.projection.v1", "projectionGeneration": "generation-1",
            "refId": "session-event:event-1:modelMarkdown", "revision": "1",
            "byteLength": "6", "offset": "0", "maxBytes": 65536,
        }
        result = {key: value for key, value in request.items() if key not in {"schema", "offset", "maxBytes"}}
        result.update(schema="transcript.content.range.v1", startOffset="0", endOffset="3", content="中", hasMore=True)
        response = urlopen.return_value.__enter__.return_value
        response.read.return_value = json.dumps(result).encode()
        self.assertEqual(request_transcript_content(request), result)
        for changes in ({"sessionId": "foreign"}, {"revision": "2"}, {"endOffset": "4"},
                        {"hasMore": False}, {"unknown": True}, {"content": "", "endOffset": "0"}):
            with self.subTest(changes=changes):
                response.read.return_value = json.dumps({**result, **changes}).encode()
                with self.assertRaisesRegex(RuntimeError, "transcript_content_response_invalid"):
                    request_transcript_content(request)

    @patch("app_core.runtime_client.urllib.request.urlopen")
    def test_page_accepts_a_tool_summary_reference(self, urlopen):
        request = {
            "schema": "runtime.transcript.page.read.v1",
            "sessionId": "session-1",
            "projectionVersion": "transcript.projection.v1",
            "projectionGeneration": "generation-1",
            "sourceHighWater": "1",
            "olderCursor": None,
        }
        page = {
            "schema": "transcript.page.v1",
            "sessionId": "session-1",
            "projectionVersion": "transcript.projection.v1",
            "projectionGeneration": "generation-1",
            "sourceHighWater": "1",
            "blocks": [
                {
                    "blockId": "tool:call-1",
                    "blockRevision": "1",
                    "orderKey": {"sourceSequence": "1", "ordinal": 0},
                    "body": {
                        "kind": "tool",
                        "callId": "call-1",
                        "toolName": "read_file",
                        "status": "running",
                        "summary": None,
                        "summaryRef": {
                            "refId": "session-event:event-1:displayTarget",
                            "revision": "1",
                            "byteLength": "70000",
                        },
                        "outputRef": None,
                    },
                }
            ],
            "olderCursor": None,
            "hasOlder": False,
            "resumeCursors": [
                {"streamId": "workspace-transcript.v1", "cursor": "1"}
            ],
        }
        response = urlopen.return_value.__enter__.return_value
        response.read.return_value = json.dumps(page).encode()

        self.assertEqual(request_transcript_page(request), page)

    @patch("app_core.runtime_client.urllib.request.urlopen")
    def test_page_response_rejects_a_malformed_nested_block(self, urlopen):
        request = {
            "schema": "runtime.transcript.page.read.v1",
            "sessionId": "session-1",
            "projectionVersion": "transcript.projection.v1",
            "projectionGeneration": "generation-1",
            "sourceHighWater": "1",
            "olderCursor": None,
        }
        response = urlopen.return_value.__enter__.return_value
        response.read.return_value = json.dumps(
            {
                "schema": "transcript.page.v1",
                "sessionId": "session-1",
                "projectionVersion": "transcript.projection.v1",
                "projectionGeneration": "generation-1",
                "sourceHighWater": "1",
                "blocks": [
                    {
                        "blockId": "message-1",
                        "blockRevision": "1",
                        "orderKey": {"sourceSequence": "1", "ordinal": 0},
                        "body": {
                            "kind": "userText",
                            "content": {"inlineContent": "hello", "sourceRef": None},
                            "unknown": True,
                        },
                    }
                ],
                "olderCursor": None,
                "hasOlder": False,
                "resumeCursors": [
                    {"streamId": "workspace-transcript.v1", "cursor": "1"}
                ],
            }
        ).encode()

        with self.assertRaisesRegex(RuntimeError, "transcript_page_response_invalid"):
            request_transcript_page(request)

    @patch("app_core.runtime_client.urllib.request.urlopen")
    def test_page_not_ready_conflict_is_exact_and_bound_to_the_request(self, urlopen):
        request = {
            "schema": "runtime.transcript.page.read.v1",
            "sessionId": "session-1",
            "projectionVersion": "transcript.projection.v1",
            "projectionGeneration": None,
            "sourceHighWater": "9",
            "olderCursor": None,
        }
        payload = {
            "error": "transcript_projection_not_ready",
            "projectionGeneration": "generation-1",
            "sourceHighWater": "9",
            "projectedHighWater": "4",
        }
        urlopen.side_effect = _http_conflict(payload)

        with self.assertRaises(TranscriptRuntimeError) as raised:
            request_transcript_page(request)
        self.assertEqual(raised.exception.payload, payload)

    @patch("app_core.runtime_client.urllib.request.urlopen")
    def test_malformed_or_unbound_not_ready_conflict_is_not_forwarded(self, urlopen):
        request = {
            "schema": "runtime.transcript.patch.read.v1",
            "sessionId": "session-1",
            "projectionVersion": "transcript.projection.v1",
            "projectionGeneration": "generation-1",
            "afterSourceHighWater": "4",
            "throughSourceHighWater": "9",
        }
        urlopen.side_effect = _http_conflict(
            {
                "error": "transcript_projection_not_ready",
                "projectionGeneration": "other-generation",
                "sourceHighWater": "9",
                "projectedHighWater": "4",
            }
        )

        with self.assertRaisesRegex(RuntimeError, "transcript_runtime_request_failed"):
            request_transcript_patches(request)

    @patch("app_core.runtime_client.urllib.request.urlopen")
    def test_view_invalidation_conflict_rejects_unknown_fields(self, urlopen):
        request = {
            "schema": "runtime.transcript.page.read.v1",
            "sessionId": "session-1",
            "projectionVersion": "transcript.projection.v1",
            "projectionGeneration": "generation-1",
            "sourceHighWater": "9",
            "olderCursor": None,
        }
        urlopen.side_effect = _http_conflict(
            {"error": "transcript_view_invalidated", "retry": True}
        )

        with self.assertRaisesRegex(RuntimeError, "transcript_runtime_request_failed"):
            request_transcript_page(request)


class TranscriptEnvelopeTests(SimpleTestCase):
    def test_generated_envelopes_and_nested_fields_are_strict(self):
        from app_core.transcript_contract import valid_transcript_structure
        folder = Path(__file__).parent / "generated"
        samples = json.loads((folder / "transcript_envelopes.samples.json").read_text(encoding="utf-8"))
        schemas = json.loads((folder / "transcript_envelopes.schema.json").read_text(encoding="utf-8"))
        self.assertEqual(set(samples), set(schemas))
        self.assertEqual(len(samples), 8)
        for name, values in samples.items():
            for value in values:
                self.assertTrue(valid_transcript_structure(name, value), name)
                for key in schemas[name]["required"]:
                    missing = dict(value)
                    del missing[key]
                    self.assertFalse(valid_transcript_structure(name, missing), (name, key))
                self.assertFalse(valid_transcript_structure(name, {**value, "unknown": 1}))
                for key in value:
                    if any(letter.isupper() for letter in key):
                        renamed = dict(value)
                        renamed["wrong_" + key] = renamed.pop(key)
                        self.assertFalse(valid_transcript_structure(name, renamed))
        page = samples["page"][0]
        patches = samples["patches"][0]
        for name, value, paths in [
            ("page", page, [("resumeCursors", 0), ("blocks", 0), ("blocks", 0, "body")]),
            ("patches", patches, [("patches", 0), ("patches", 0, "removals", 0)]),
        ]:
            for path in paths:
                mutated = copy.deepcopy(value)
                child = mutated
                for key in path:
                    child = child[key]
                child["unknown"] = True
                self.assertFalse(valid_transcript_structure(name, mutated), path)

    def test_request_optional_null_fields_follow_rust_deserialization(self):
        from app_core.transcript_contract import valid_transcript_structure
        request = {"schema": "runtime.transcript.page.read.v1", "sessionId": "s",
                   "projectionVersion": "v", "sourceHighWater": "0"}
        self.assertTrue(valid_transcript_structure("pageRequest", request))
        self.assertTrue(valid_transcript_structure("pageRequest", {
            **request, "olderCursor": None, "projectionGeneration": None}))
        self.assertFalse(valid_transcript_structure("pageRequest", {**request, "olderCursor": False}))

    def test_serialized_responses_keep_semantic_guards(self):
        from app_core.runtime_client import _validate_transcript_conflict
        samples = json.loads((Path(__file__).parent / "generated/transcript_envelopes.samples.json").read_text(encoding="utf-8"))
        cases = [(request_transcript_page, "page", {**samples["pageRequest"][0], "sourceHighWater": "1"}),
                 (request_transcript_patches, "patches", samples["patchRequest"][0]),
                 (request_transcript_content, "content", samples["contentRequest"][0])]
        for read, name, request in cases:
            response = samples[name][0]
            with patch("app_core.runtime_client._request_transcript_read", return_value=response):
                self.assertEqual(read(request), response)
            mutations = [{**response, "sessionId": "foreign"}]
            if name == "page":
                mutations.append({**response, "hasOlder": True})
            elif name == "patches":
                mutations.append({**response, "nextSourceHighWater": "0"})
            else:
                mutations.append({**response, "endOffset": "2"})
            for invalid in mutations:
                with patch("app_core.runtime_client._request_transcript_read", return_value=invalid):
                    with self.assertRaises(RuntimeError):
                        read(request)
        conflict = samples["projectionNotReady"][0]
        request = samples["patchRequest"][0]
        self.assertEqual(_validate_transcript_conflict(conflict, request), conflict)
        self.assertIsNone(_validate_transcript_conflict({**conflict, "projectedHighWater": "1"}, request))

    def test_response_envelopes_reject_missing_extra_and_wrong_field_names(self):
        cases = [
            (request_transcript_page,
             {"sessionId": "s", "projectionVersion": "v", "sourceHighWater": "0"},
             {"schema": "transcript.page.v1", "sessionId": "s", "projectionVersion": "v",
              "projectionGeneration": "g", "sourceHighWater": "0", "blocks": [],
              "olderCursor": None, "hasOlder": False, "resumeCursors": []}),
            (request_transcript_patches,
             {"sessionId": "s", "projectionVersion": "v", "projectionGeneration": "g",
              "afterSourceHighWater": "0", "throughSourceHighWater": "0"},
             {"schema": "transcript.patch.page.v1", "sessionId": "s", "projectionVersion": "v",
              "projectionGeneration": "g", "throughSourceHighWater": "0", "patches": [],
              "nextSourceHighWater": "0", "hasMore": False}),
            (request_transcript_content,
             {"sessionId": "s", "projectionVersion": "v", "projectionGeneration": "g",
              "refId": "r", "revision": "1", "byteLength": "1", "offset": "0", "maxBytes": 1},
             {"schema": "transcript.content.range.v1", "sessionId": "s", "projectionVersion": "v",
              "projectionGeneration": "g", "refId": "r", "revision": "1", "byteLength": "1",
              "startOffset": "0", "endOffset": "1", "content": "x", "hasMore": False}),
        ]
        for read, request, response in cases:
            with patch("app_core.runtime_client._request_transcript_read", return_value=response):
                self.assertEqual(read(request), response)
            invalid = [{**response, "unknown": True}]
            for key in response:
                missing = dict(response)
                del missing[key]
                invalid.append(missing)
            renamed = dict(response)
            renamed["session_id"] = renamed.pop("sessionId")
            invalid.append(renamed)
            for value in invalid:
                with self.subTest(reader=read.__name__, value=value), patch(
                    "app_core.runtime_client._request_transcript_read", return_value=value
                ):
                    with self.assertRaises(RuntimeError):
                        read(request)
