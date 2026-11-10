from copy import deepcopy
from types import SimpleNamespace
from unittest.mock import patch

from django.test import SimpleTestCase

from .models import AgentRun, Session, SessionCitationProjection, Workspace
from .source_associations import merge_read_citation_projection, read_citation_projections, reconstruct_source_event, validate_source_association


class SourceAssociationContractTests(SimpleTestCase):
    def setUp(self):
        self.identity = {"sourceRef": "sourceObject:source-one", "revision": "1", "sha256": "sha256:" + "a" * 64}
        self.payload = {"associationId": "source:read-one", "inputRef": "input-one", "sourceIdentity": self.identity,
            "sourceToolName": "read", "sourceToolCallId": "read-call", "range": {"startLine": 1, "endLine": 2}}

    def test_generic_source_contract_rejects_unknown_fields_invalid_hashes_and_ranges(self):
        validate_source_association(self.payload)
        for invalid in [
            {**self.payload, "extra": True},
            {**self.payload, "sourceIdentity": {**self.identity, "ownerKind": "sourceObject"}},
            {**self.payload, "sourceIdentity": {**self.identity, "sourceRef": "/private/file"}},
            {**self.payload, "sourceIdentity": {**self.identity, "sha256": "sha256:invalid"}},
            {**self.payload, "range": {"startLine": 3, "endLine": 2}},
            {**self.payload, "range": {"startLine": True, "endLine": 2}},
            {**self.payload, "range": {"startLine": 1, "endLine": 2**64}},
        ]:
            with self.subTest(invalid=invalid), self.assertRaisesRegex(ValueError, "source_association_invalid"):
                validate_source_association(invalid)

    def test_historical_reconstruction_preserves_row_identity_and_uses_content_revision_when_generation_absent(self):
        old = {"citationId": "citation:" + "b" * 64, "inputRef": "input-one", "ownerKind": "sourceObject",
            "ownerRef": "source-one", "displayName": "Source", "evidenceKind": "workspaceSource", "ownerSha256": self.identity["sha256"], "sourceToolName": "read",
            "sourceToolCallId": "read-call", "locator": {"pageStart": 2, "pageEnd": 3}}
        wire = {"type": "citation_recorded", "eventId": "event-one", "sequence": 4, "turnId": "turn-one",
            "agentRunId": "run-one", "payload": old}
        before = deepcopy(wire)
        rebuilt = reconstruct_source_event(wire)
        self.assertEqual(wire, before)
        self.assertEqual({key: value for key, value in rebuilt.items() if key not in {"type", "payload"}},
            {key: value for key, value in wire.items() if key not in {"type", "payload"}})
        self.assertEqual(rebuilt["type"], "source_associated")
        self.assertEqual(rebuilt["payload"]["associationId"], old["citationId"])
        self.assertEqual(rebuilt["payload"]["sourceIdentity"]["revision"], old["ownerSha256"])
        self.assertNotIn("range", rebuilt["payload"])
        typed = {**old, "ownerGeneration": 3, "representationId": "representation:sha256:" + "c" * 64,
            "specDigest": "sha256:" + "d" * 64, "evidenceSha256": "sha256:" + "e" * 64, "locator": {"kind": "textSpan", "pageStart": 4,
            "pageEnd": 4, "startLine": 7, "endLine": 8, "startByte": 10, "endByte": 25}}
        rebuilt = reconstruct_source_event({**wire, "payload": typed})
        self.assertEqual(rebuilt["payload"]["range"], {"startLine": 7, "endLine": 8})
        self.assertEqual(rebuilt["payload"]["sourceIdentity"]["revision"], "3")
        for malformed in [
            {**old, "ownerKind": "unknown"}, {**old, "evidenceKind": "unknown"},
            {**old, "citationId": "citation:invalid"}, {**old, "unknown": True},
            {**old, "ownerGeneration": None},
            {**old, "locator": {"startLine": 2, "endLine": 1}},
            {**old, "locator": {"kind": "pageRegion", "page": 1, "bbox": [1, 0, 1, 100]}},
            {**typed, "ownerGeneration": None}, {**typed, "specDigest": "sha256:invalid"},
        ]:
            with self.subTest(malformed=malformed), self.assertRaisesRegex(ValueError, "stored_citation"):
                reconstruct_source_event({**wire, "payload": malformed})

    def test_file_citation_requires_builtin_success_matching_identity_and_citable_product_policy(self):
        workspace = Workspace(id="workspace-one")
        session = Session(id="session-one", workspace=workspace)
        run = AgentRun(id="run-one", session=session, workspace=workspace)
        source = {"inputRef": "input-one", "ownerRef": "source-one", "ownerKind": "sourceObject",
            "ownerSha256": self.identity["sha256"], "ownerGeneration": 1, "displayName": "Source",
            "citationAllowed": True, "evidenceKind": "workspaceSource"}
        def record(sequence, kind, payload):
            return SimpleNamespace(sequence=sequence, payload={"type": kind, "payload": payload,
                "turnId": "turn-one", "sessionId": session.pk, "agentRunId": run.pk})
        records = [record(1, "tool_call", {"callId": "read-call", "toolName": "read", "providerId": "centaeris.builtin"}),
            record(2, "tool_result", {"callId": "read-call", "toolName": "read", "resultState": "successWithOutput", "outputComplete": True}),
            record(3, "source_associated", self.payload)]
        originals = deepcopy(records)
        with patch("app_core.source_associations._authorized_sources", return_value={"input-one": source}):
            citation, = read_citation_projections(run, records)
            self.assertTrue(citation.citationId.startswith("citation:"))
            self.assertEqual(citation.locator, self.payload["range"])
            for index, path, value in [
                (0, ("payload", "providerId"), "external.mcp"),
                (1, ("payload", "resultState"), "failed"),
                (1, ("payload", "outputComplete"), False),
                (1, ("turnId",), "foreign-turn"),
                (2, ("agentRunId",), "foreign-run"),
                (2, ("payload", "sourceIdentity", "revision"), "2"),
                (2, ("payload", "sourceIdentity", "sha256"), "sha256:" + "b" * 64),
                (2, ("payload", "sourceIdentity", "sourceRef"), "userLibraryObject:source-one"),
            ]:
                records = deepcopy(originals)
                target = records[index].payload
                for key in path[:-1]:
                    target = target[key]
                target[path[-1]] = value
                with self.subTest(path=path, value=value):
                    self.assertEqual(read_citation_projections(run, records), [])
            source["citationAllowed"] = False
            self.assertEqual(read_citation_projections(run, deepcopy(originals)), [])
            source["citationAllowed"] = True
            source["ownerKind"] = "artifact"
            self.assertEqual(read_citation_projections(run, deepcopy(originals)), [])

    def direct_read_projections(self):
        original = {"citationId": "citation:" + "b" * 64, "inputRef": "input-one", "ownerRef": "source-one",
            "ownerKind": "sourceObject", "ownerSha256": self.identity["sha256"], "displayName": "Source",
            "evidenceKind": "workspaceSource", "sourceToolName": "read", "sourceToolCallId": "old-call",
            "locator": {"startLine": 1, "endLine": 2}}
        previous = SessionCitationProjection(workspace_id="workspace-one", session_id="session-one", agent_run_id="run-one",
            sequence=4, **deepcopy(original))
        repeated = SessionCitationProjection(workspace_id="workspace-one", session_id="session-one", agent_run_id="run-one",
            sequence=7, **{**deepcopy(original), "sourceToolCallId": "new-call", "ownerGeneration": 9})
        return original, previous, repeated

    def test_upgraded_reread_retains_original_direct_evidence_with_uncaptured_generation(self):
        original, previous, repeated = self.direct_read_projections()
        before = deepcopy(original)
        citations = {previous.citationId: previous}
        merge_read_citation_projection(citations, {previous.citationId: original}, repeated)
        self.assertIs(citations[previous.citationId], previous)
        self.assertEqual(previous.sequence, 4)
        self.assertEqual(previous.sourceToolCallId, "old-call")
        self.assertEqual(previous.ownerGeneration, 1)
        self.assertEqual(repeated.ownerGeneration, 9)
        self.assertEqual(original, before)

    def test_upgraded_reread_does_not_merge_conflicting_or_nonhistorical_evidence(self):
        original, previous, repeated = self.direct_read_projections()
        for field, value in [
            ("inputRef", "other-input"), ("ownerKind", "userLibraryObject"), ("ownerRef", "other-source"),
            ("ownerSha256", "sha256:" + "c" * 64), ("displayName", "Other source"),
            ("evidenceKind", "userProvided"), ("sourceToolName", "search_materials"),
            ("locator", {"startLine": 2, "endLine": 3}), ("session_id", "other-session"),
            ("representationId", "representation:sha256:" + "d" * 64),
        ]:
            candidate = deepcopy(repeated)
            setattr(candidate, field, value)
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, "material citation identity conflict"):
                merge_read_citation_projection({previous.citationId: previous}, {previous.citationId: original}, candidate)
        for historical in [None, {**original, "sourceToolName": "search_materials"},
            {**original, "locator": {"pageStart": 1, "pageEnd": 2}},
            {**original, "locator": {"kind": "textSpan", "startLine": 1, "endLine": 2}},
            {**original, "representationId": "representation:sha256:" + "d" * 64},
            {**original, "specDigest": "sha256:" + "e" * 64},
            {**original, "evidenceSha256": "sha256:" + "f" * 64},
            {**original, "ownerGeneration": 8}]:
            historical_payloads = {previous.citationId: historical} if historical is not None else {}
            with self.subTest(historical=historical), self.assertRaisesRegex(ValueError, "material citation identity conflict"):
                merge_read_citation_projection({previous.citationId: previous}, historical_payloads, repeated)
