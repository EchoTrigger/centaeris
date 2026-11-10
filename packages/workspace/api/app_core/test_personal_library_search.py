"""Full authorized-content search and independent bounded processing coverage."""
from datetime import datetime, timezone
from types import SimpleNamespace
from unittest.mock import patch
from uuid import uuid4

from django.test import SimpleTestCase, TestCase

from .material_contract import KnowledgeError, sha256_bytes
from .material_identity import representation_id
from . import material_library_search as search
from .assets import tombstone_stored_object
from .library_notes import save_note
from .models import (DerivedRepresentation, KnowledgeSegment, MaterialEvidenceReceipt, MaterialOperation,
                     MaterialProcessingTask, MaterialResultSnapshot, ProcessingSpecification, SessionEvent, UserLibraryObject)
from .test_personal_library_materials import PersonalLibraryMaterialFixture


class PersonalLibrarySearchCursorTests(SimpleTestCase):
    def setUp(self):
        self.access = SimpleNamespace(agent_run=SimpleNamespace(pk="run-a"),
                                      authorization_digest="sha256:" + "a" * 64, spec_digest="sha256:" + "b" * 64)
        self.selection = search._selection_digest(self.access, "all", None, "query", "relevance")
        self.as_of = datetime(2026, 11, 2, 8, 9, 10, 123456, tzinfo=timezone.utc)

    def test_rank_cursor_preserves_exact_timestamp_and_rejects_changed_authority_or_selection(self):
        token = search._cursor(search.SEARCH_CURSOR_SCHEMA, self.selection, self.as_of,
                               [5, self.as_of.isoformat(), "segment:one"])
        decoded = search._decode_cursor(token, search.SEARCH_CURSOR_SCHEMA, self.selection)
        self.assertEqual(decoded["asOf"], self.as_of)
        self.assertEqual(decoded["after"], (5, self.as_of, "segment:one"))
        for field, value in [("query", "different"), ("scope", "attached"), ("refs", []), ("ranking", "recent")]:
            choices = {"scope": "all", "refs": None, "query": "query", "ranking": "relevance", field: value}
            selection = search._selection_digest(self.access, choices["scope"], choices["refs"], choices["query"], choices["ranking"])
            with self.subTest(field=field), self.assertRaises(KnowledgeError):
                search._decode_cursor(token, search.SEARCH_CURSOR_SCHEMA, selection)
        for attribute in ("authorization_digest", "spec_digest"):
            changed = SimpleNamespace(**vars(self.access))
            setattr(changed, attribute, "sha256:" + "c" * 64)
            selection = search._selection_digest(changed, "all", None, "query", "relevance")
            with self.subTest(attribute=attribute), self.assertRaises(KnowledgeError):
                search._decode_cursor(token, search.SEARCH_CURSOR_SCHEMA, selection)
        changed = SimpleNamespace(**vars(self.access), )
        changed.agent_run = SimpleNamespace(pk="run-b")
        with self.assertRaises(KnowledgeError):
            search._decode_cursor(token, search.SEARCH_CURSOR_SCHEMA,
                                  search._selection_digest(changed, "all", None, "query", "relevance"))

    def test_processing_and_search_cursors_are_distinct_and_tampering_fails(self):
        token = search._cursor(search.PROCESSING_CURSOR_SCHEMA, self.selection, self.as_of, "library|object-a")
        self.assertEqual(search._decode_cursor(token, search.PROCESSING_CURSOR_SCHEMA, self.selection)["after"], "library|object-a")
        for altered, schema in [(token, search.SEARCH_CURSOR_SCHEMA), (token + "tampered", search.PROCESSING_CURSOR_SCHEMA)]:
            with self.subTest(schema=schema), self.assertRaises(KnowledgeError):
                search._decode_cursor(altered, schema, self.selection)

    def test_zip_mime_is_unsupported_but_text_title_and_unknown_formats_are_not_guessed(self):
        for name, mime, expected in [
            ("archive.zip", "application/zip", "material_format_unsupported"),
            ("notes.zip", "text/markdown", None),
            ("unknown.bin", "application/octet-stream", None),
        ]:
            owner = SimpleNamespace(displayName=name, contentType=mime, sizeBytes=1)
            with self.subTest(name=name):
                self.assertEqual(search._preparation_error(owner, 64 * 1024 * 1024), expected)


class PersonalLibrarySearchArgumentTests(SimpleTestCase):
    def test_invalid_search_arguments_fail_before_scope_or_database_access(self):
        with patch.object(search, "_scope") as scope:
            for changes in (
                {"query": ""}, {"query": " \n"}, {"query": "界" * 683},
                {"ranking": "unknown"}, {"limit": True}, {"limit": False},
                {"limit": 0}, {"limit": 21}, {"limit": "1"},
            ):
                with self.subTest(changes=changes), self.assertRaisesRegex(
                    KnowledgeError, "knowledge_search_arguments_invalid"
                ):
                    search.search_library_materials(object(), **{"query": "policy", **changes})
            scope.assert_not_called()

    def test_unknown_search_arguments_fail_closed(self):
        for changes in ({"unexpected": True}, {"schema": "unknown"}, {"date_range": None}):
            with self.subTest(changes=changes), self.assertRaises(TypeError):
                search.search_library_materials(object(), query="policy", **changes)


class PersonalLibrarySearchTests(PersonalLibraryMaterialFixture, TestCase):
    def setUp(self):
        super().setUp()
        self.agent_run = self.new_run()
        self.search_access = self.access(self.agent_run)
        self.spec = ProcessingSpecification.objects.create(specDigest=self.search_access.spec_digest,
                                                          payload=self.search_access.processing_specification)
        self.index(self.note, [self.note.noteHead.markdown])

    def index(self, owner, texts):
        identity = {"ownerKind": "userLibraryObject", "ownerId": owner.pk,
                    "generation": owner.contentGeneration, "sha256": owner.sha256}
        key = representation_id(identity, self.search_access.spec_digest)
        canonical = "\n".join(texts)
        representation = DerivedRepresentation.objects.create(representationId=key, ownerKind="userLibraryObject",
            ownerId=owner.pk, ownerContentGeneration=owner.contentGeneration, ownerSha256=owner.sha256,
            processingSpecification=self.spec, pageCount=1, canonicalTextKey=f"synthetic/{key}/canonical.md",
            canonicalTextSizeBytes=len(canonical.encode()), canonicalTextSha256=sha256_bytes(canonical.encode()), manifest={"pages": []})
        offset, line = 0, 1
        segments = []
        for ordinal, text in enumerate(texts):
            size = len(text.encode())
            segments.append(KnowledgeSegment(segmentId="segment:" + sha256_bytes(f"{key}:{ordinal}".encode())[7:],
                representation=representation, ordinal=ordinal, boundedText=text, textSha256=sha256_bytes(text.encode()),
                locator={"kind": "textSpan", "startByte": offset, "endByte": offset + size,
                         "startLine": line, "endLine": line + text.count("\n") - int(text.endswith("\n")),
                         "pageStart": 1, "pageEnd": 1}))
            offset += size + 1
            line += text.count("\n") + 1
        KnowledgeSegment.objects.bulk_create(segments)
        return representation

    def query(self, text="exact", **kwargs):
        return search.search_library_materials(self.search_access, query=text, **kwargs)

    def failed_task(self, owner):
        identity = {"ownerKind": "userLibraryObject", "ownerId": owner.pk,
                    "generation": owner.contentGeneration, "sha256": owner.sha256}
        return MaterialProcessingTask.objects.create(representationId=representation_id(identity, self.search_access.spec_digest),
            processingSpecification=self.search_access.processing_specification, status="failed", errorCode="material_processor_failed",
            payload={"schema": "workspace.material.processing.v1", "inputIdentity": identity,
                     "specDigest": self.search_access.spec_digest, "sizeBytes": owner.sizeBytes})

    def test_default_all_searches_more_than_128_library_objects_with_ranked_keyset_pages(self):
        expected = set()
        for index in range(129):
            owner = self.new_note(f"Indexed {index}", "needle")
            self.index(owner, ["needle"])
            expected.add(owner.pk)
        cursor = None
        observed = []
        while True:
            result = self.query("needle", cursor=cursor, limit=20)
            observed.extend(hit["ownerRef"] for hit in result["hits"])
            self.assertTrue(result["coverage"]["complete"])
            cursor = result["nextSearchCursor"]
            if cursor is None:
                break
        self.assertEqual(len(observed), 129)
        self.assertEqual(set(observed), expected)

    def test_one_large_representation_is_not_limited_to_10000_segments(self):
        texts = ["needle"] * 10001
        owner = self.new_note("Many segments", "\n".join(texts))
        self.index(owner, texts)
        result = self.query("needle", limit=20)
        self.assertEqual(len(result["hits"]), 20)
        self.assertIsNotNone(result["nextSearchCursor"])
        self.assertTrue(result["coverage"]["complete"])

    def test_unicode_casefold_matches_sharp_s_greek_sigma_and_chinese_without_changing_evidence(self):
        for index, (content, query, score) in enumerate([("Straße", "STRASSE", 3), ("ΟΣ ος οσ", "οσ", 5), ("中文片段", "中文", 3)]):
            owner = self.new_note(f"Unicode {index}", content)
            self.index(owner, [content])
            result = self.query(query)
            hit = next(item for item in result["hits"] if item["ownerRef"] == owner.pk)
            self.assertEqual((hit["content"], hit["score"]), (content, score))

    def test_ranked_pages_preserve_full_evidence_and_recent_order(self):
        older_text = "policy policy\n" + "😀" * 3000
        newer_text = "policy\n" + "😀" * 3000
        older = self.new_note("Older", older_text)
        newer = self.new_note("Newer", newer_text + "\nunrelated\n" + newer_text)
        self.index(older, [older_text])
        newer_rep = self.index(newer, [newer_text, "unrelated", newer_text])
        UserLibraryObject.objects.filter(pk=older.pk).update(updatedAt=datetime(2026, 1, 1, tzinfo=timezone.utc))
        UserLibraryObject.objects.filter(pk=newer.pk).update(updatedAt=datetime(2026, 1, 2, tzinfo=timezone.utc))
        tied = list(KnowledgeSegment.objects.filter(representation=newer_rep).exclude(boundedText="unrelated")
                    .order_by("segmentId").values_list("segmentId", flat=True))
        first = self.query("policy", ranking="recent", limit=2)
        self.assertEqual([hit["segmentId"] for hit in first["hits"]], tied)
        second = self.query("policy", ranking="recent", limit=2, cursor=first["nextSearchCursor"])
        self.assertEqual([hit["ownerRef"] for hit in second["hits"]], [older.pk])
        self.assertIsNone(second["nextSearchCursor"])
        relevance = self.query("policy", ranking="relevance")
        self.assertEqual([hit["ownerRef"] for hit in relevance["hits"]], [older.pk, newer.pk, newer.pk])
        for hit in relevance["hits"]:
            expected = older_text if hit["ownerRef"] == older.pk else newer_text
            self.assertEqual(hit["content"], expected)
            self.assertEqual(hit["evidenceSha256"], sha256_bytes(expected.encode()))
            self.assertEqual(hit["locator"]["endByte"] - hit["locator"]["startByte"], len(expected.encode()))
            self.assertTrue(hit["citationAllowed"])
        self.assertNotIn("storageKey", str(relevance))

    def test_explicit_empty_selection_is_empty_and_old_all_scope_remains_attached(self):
        empty = self.query(input_refs=[])
        self.assertEqual((empty["hits"], empty["coverage"]["totalMaterials"], empty["operations"]), ([], 0, []))
        self.assertTrue(empty["coverage"]["complete"])
        self.attach(self.note)
        old = self.new_run(personal=False)
        result = search.search_library_materials(self.access(old), query="exact")
        self.assertEqual(result["coverage"]["totalMaterials"], 1)
        self.assertEqual([hit["ownerRef"] for hit in result["hits"]], [self.note.pk])
        with self.assertRaises(KnowledgeError):
            search.search_library_materials(self.access(old), query="exact", scope="library")

    def test_search_excludes_other_users_stale_versions_and_deleted_materials(self):
        foreign = self.new_note("Foreign", "private needle", owner=self.other)
        stale = self.new_note("Changed", "private needle")
        deleted = self.new_note("Deleted", "private needle")
        old_ref = search.library_descriptor(self.agent_run, stale)["inputRef"]
        for owner in (foreign, stale, deleted):
            self.index(owner, ["private needle"])
        save_note(self.user, stale.pk, save_id=uuid4().hex, base_generation=stale.contentGeneration,
                  display_name=stale.displayName, markdown="current version")
        tombstone_stored_object(deleted, deleted_by=self.user)
        result = self.query("private needle")
        self.assertEqual(result["hits"], [])
        self.assertEqual(result["coverage"]["indexedMaterials"], 1)
        for ref in (old_ref, search.library_descriptor(self.agent_run, foreign)["inputRef"]):
            with self.subTest(ref=ref), self.assertRaises(KnowledgeError):
                self.query("private needle", input_refs=[ref])

    def test_missing_preparation_advances_independently_in_bounded_pages(self):
        for index in range(21):
            self.new_note(f"Missing {index}", "not indexed")
        first = self.query()
        self.assertEqual((first["coverage"]["unprocessedMaterials"], len(first["operations"])), (21, 20))
        self.assertFalse(first["coverage"]["complete"])
        self.assertEqual(len(first["hits"]), 1)
        self.assertIsNotNone(first["processingCursor"])
        second = self.query(processing_cursor=first["processingCursor"])
        self.assertEqual(len(second["operations"]), 1)
        self.assertIsNone(second["processingCursor"])
        self.assertEqual(MaterialOperation.objects.count(), 21)

    def test_failed_and_zip_do_not_block_hits_or_reset_failed_tasks(self):
        failed = self.new_note("Failed", "missing content")
        task = self.failed_task(failed)
        UserLibraryObject.objects.create(owner=self.user, displayName="archive.zip", objectKind="file", contentType="application/zip",
            sizeBytes=4, sha256=sha256_bytes(b"zip!"), contentGeneration=1, status="ready")
        text = self.new_note("Markdown title.zip", "still a text note")
        result = self.query()
        counts = result["coverage"]
        self.assertEqual((counts["totalMaterials"], counts["indexedMaterials"], counts["failedMaterials"],
                          counts["unsupportedMaterials"], counts["unprocessedMaterials"]), (4, 1, 1, 1, 1))
        self.assertFalse(counts["complete"])
        self.assertEqual(len(result["hits"]), 1)
        self.assertCountEqual([item["errorCode"] for item in result["processingFailures"]],
                              ["material_processor_failed", "material_format_unsupported"])
        self.assertEqual(len(result["operations"]), 1)
        task.refresh_from_db()
        self.assertEqual((task.status, task.attemptCount), ("failed", 0))
        self.assertEqual(result["operations"][0]["inputRef"], search.library_descriptor(self.agent_run, text)["inputRef"])

    def test_new_representation_requires_refresh_of_an_existing_search_window(self):
        for index in range(2):
            owner = self.new_note(f"Before {index}", "needle")
            self.index(owner, ["needle"])
        first = self.query("needle", limit=1)
        later = self.new_note("After snapshot", "needle needle")
        self.index(later, ["needle needle"])
        second = self.query("needle", limit=1, cursor=first["nextSearchCursor"])
        self.assertNotIn(later.pk, [hit["ownerRef"] for hit in second["hits"]])
        self.assertTrue(second["coverage"]["refreshRequired"])
        self.assertFalse(second["coverage"]["complete"])
        self.assertGreater(second["coverage"]["indexedMaterials"], second["coverage"]["searchedMaterials"])
        refreshed = self.query("needle", limit=1)
        self.assertEqual(refreshed["hits"][0]["ownerRef"], later.pk)

    def test_ready_call_replay_reuses_saved_result_cursors_and_receipt_without_new_operations(self):
        from . import platform_mcp
        from .material_access import MaterialAccessContext
        arguments = {"query": "exact", "scope": "library", "limit": 1}
        call_id = "search-replay"
        SessionEvent.objects.create(eventId="search-replay-event", workspace=self.workspace, session=self.session,
            agent_run=self.agent_run, sequence=1, agent_run_sequence=1, createdAtMs=1, projects_to_agent_run_stream=True,
            payload={"type": "tool_call", "turnId": self.agent_run.turn_id, "payload": {
                "providerId": "workspace.materials", "toolName": "search_materials", "callId": call_id, "normalizedInput": arguments}})
        context = MaterialAccessContext(self.agent_run.pk, self.agent_run.authorization.digest,
                                       self.search_access.processing_specification, self.search_access.spec_digest)
        marker = platform_mcp.request_call_id.set(call_id)
        try:
            first = platform_mcp.execute_tool(context, "search_materials", arguments)
            added = self.new_note("New index", "exact exact")
            self.index(added, ["exact exact"])
            with patch.object(search, "search_library_materials", side_effect=AssertionError("replay searched again")), \
                    patch.object(search, "create_operations", side_effect=AssertionError("replay enqueued work")):
                second = platform_mcp.execute_tool(context, "search_materials", arguments)
            self.assertEqual(second, first)
        finally:
            platform_mcp.request_call_id.reset(marker)
        self.assertEqual(MaterialResultSnapshot.objects.count(), 1)
        self.assertEqual(MaterialEvidenceReceipt.objects.count(), 1)
        self.assertEqual(MaterialOperation.objects.count(), 0)
