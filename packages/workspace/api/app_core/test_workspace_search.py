"""Browser search consumes current owner/representation identity and current ACLs."""
from tempfile import TemporaryDirectory
from urllib.parse import parse_qs, urlparse

from django.contrib.auth import get_user_model
from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.test import TestCase, override_settings
from django.utils import timezone

from .material_contract import sha256_bytes
from .material_identity import processing_spec_digest, representation_id
from .models import (DerivedRepresentation, Issue, IssueDeliverable, IssueExecution, KnowledgeSegment,
                     ProcessingSpecification, Source, SourceGrant, SourceObject, UserLibraryObject,
                     WorkspaceGroup, WorkspaceMembership)
from .session_record_test_fixtures import SessionRecordFixture
from .test_platform_mcp import processing_specification


class WorkspaceSearchTests(SessionRecordFixture, TestCase):
    def setUp(self):
        super().setUp()
        directory = TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        options = override_settings(MEDIA_ROOT=directory.name)
        options.enable()
        self.addCleanup(options.disable)
        self.url = f"/api/workspaces/{self.workspace.pk}/search"
        specification = processing_specification()
        self.spec = ProcessingSpecification.objects.create(specDigest=processing_spec_digest(specification), payload=specification)
        self.source = Source.objects.create(workspace=self.workspace, sourceType="uploadedFile", name="Policies",
                                            status="ready", createdBy=self.user)
        self.source_object = SourceObject.objects.create(workspace=self.workspace, source=self.source,
            objectType="file", displayPath="rules.txt", displayName="rules.txt", contentType="text/plain",
            sizeBytes=5, sha256=sha256_bytes(b"rules"), storageKey="source/rules.txt", sourceVersion="v1", status="ready")

    def library(self, name="document.txt", text="Original material", owner=None):
        return UserLibraryObject.objects.create(owner=owner or self.user, objectKind="file", displayName=name,
            contentType="text/plain", status="ready", sizeBytes=len(text.encode()), sha256=sha256_bytes(text.encode()),
            storageKey=f"library/{name}")

    def index(self, owner, text, kind="userLibraryObject", start_line=1):
        key = representation_id({"ownerKind": kind, "ownerId": owner.pk, "generation": owner.contentGeneration,
                                 "sha256": owner.sha256}, self.spec.pk)
        content = text.encode()
        stored = default_storage.save(f"search/{key}/canonical.txt", ContentFile(content))
        representation = DerivedRepresentation.objects.create(representationId=key, ownerKind=kind, ownerId=owner.pk,
            ownerContentGeneration=owner.contentGeneration, ownerSha256=owner.sha256, processingSpecification=self.spec,
            pageCount=1, canonicalTextKey=stored, canonicalTextSizeBytes=len(content), canonicalTextSha256=sha256_bytes(content), manifest={"pages": []})
        KnowledgeSegment.objects.create(segmentId="segment:" + sha256_bytes(key.encode())[7:], representation=representation,
            ordinal=0, boundedText=text, textSha256=sha256_bytes(content),
            locator={"kind": "textSpan", "startByte": 0, "endByte": len(content), "startLine": start_line,
                     "endLine": start_line + text.count("\n"), "pageStart": None, "pageEnd": None})
        return representation

    def search(self, query, **extra):
        return self.client.get(self.url, {"query": query, **extra})

    def test_body_search_has_bounded_snippets_and_current_private_owner(self):
        own = self.library()
        self.index(own, "prefix " + "财务授权" + " trailing " * 100)
        self.index(self.library("other.txt", owner=self.other), "财务授权")
        response = self.search("财务授权")
        self.assertEqual(response.status_code, 200, response.content)
        rows = response.json()["matches"]
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["id"], "userLibraryObject:" + own.pk)
        self.assertIn("财务授权", rows[0]["snippet"])
        self.assertLessEqual(len(rows[0]["snippet"]), 240)
        self.assertEqual(parse_qs(urlparse(rows[0]["href"]).query)["offset"], ["0"])
        self.client.force_login(self.other)
        self.assertEqual(len(self.search("财务授权").json()["matches"]), 1)
        self.assertNotIn(own.pk, self.search("财务授权").content.decode())

    def test_name_search_does_not_claim_unindexed_content(self):
        owner = self.library("未索引资料.txt")
        result = self.search("未索引").json()["matches"]
        self.assertEqual(result[0]["id"], "userLibraryObject:" + owner.pk)
        self.assertEqual(result[0]["snippet"], "")
        self.assertEqual(self.search("Original material").json()["matches"], [])

    def test_source_acl_is_checked_for_search_and_each_original_content_read(self):
        representation = self.index(self.source_object, "交通费核查依据\n第二行", "sourceObject", start_line=17)
        group = WorkspaceGroup.objects.create(workspace=self.workspace, name="Readers", kind="custom", createdBy=self.user)
        group.members.add(WorkspaceMembership.objects.get(workspace=self.workspace, user=self.other))
        grant = SourceGrant.objects.create(workspace=self.workspace, source=self.source, workspaceGroup=group,
                                          accessLevel="read", createdBy=self.user)
        self.client.force_login(self.other)
        rows = self.search("交通费").json()["matches"]
        self.assertEqual(len(rows), 1)
        self.assertEqual(parse_qs(urlparse(rows[0]["href"]).query)["offset"], ["16"])
        url = f"{self.url}/materials/sourceObject/{self.source_object.pk}"
        read = self.client.get(url, {"representationId": representation.pk})
        self.assertEqual(read.status_code, 200, read.content)
        self.assertEqual(read.json()["content"], "交通费核查依据\n第二行")
        grant.delete()
        self.assertEqual(self.search("交通费").json()["matches"], [])
        self.assertEqual(self.client.get(url, {"representationId": representation.pk}).status_code, 404)

    def test_changed_deleted_or_stale_representation_cannot_supply_body_hits(self):
        representation = self.index(self.source_object, "早期授权正文", "sourceObject")
        self.assertEqual(len(self.search("早期授权").json()["matches"]), 1)
        self.source_object.contentGeneration += 1
        self.source_object.save(update_fields=["contentGeneration"])
        self.assertEqual(self.search("早期授权").json()["matches"], [])
        url = f"{self.url}/materials/sourceObject/{self.source_object.pk}"
        self.assertEqual(self.client.get(url, {"representationId": representation.pk}).status_code, 404)
        self.index(self.source_object, "更新授权正文", "sourceObject")
        self.assertEqual(len(self.search("更新授权").json()["matches"]), 1)
        Source.objects.filter(pk=self.source.pk).update(status="deleted", deletedAt=timezone.now(), deletedBy=self.user, deletedFromStatus="ready")
        self.assertEqual(self.search("更新授权").json()["matches"], [])
        self.assertEqual(self.client.get(url).status_code, 404)

    def test_literal_queries_and_cursor_cannot_cross_query_user_or_membership(self):
        for index in range(3):
            self.index(self.library(f"literal-{index}.txt"), "ApiToken_100% 权限")
        self.assertEqual(len(self.search("apitoken_100%").json()["matches"]), 3)
        self.assertEqual(self.search("' OR 1=1 --").json()["matches"], [])
        first = self.search("权限", limit=2).json()
        self.assertEqual(len(first["matches"]), 2)
        second = self.search("权限", limit=2, cursor=first["nextCursor"]).json()
        self.assertEqual(len(second["matches"]), 1)
        self.assertFalse(set(row["id"] for row in first["matches"]) & set(row["id"] for row in second["matches"]))
        self.assertEqual(self.search("other", limit=2, cursor=first["nextCursor"]).status_code, 400)
        self.client.force_login(self.other)
        self.assertEqual(self.search("权限", limit=2, cursor=first["nextCursor"]).status_code, 400)
        WorkspaceMembership.objects.filter(workspace=self.workspace, user=self.other).delete()
        self.assertEqual(self.search("权限").status_code, 404)

    def test_submitted_summary_requires_recipient_or_author_and_current_issue_visibility(self):
        issue = Issue.objects.create(workspace=self.workspace, creator=self.user, assignee=self.other,
                                     title="Travel claim", visibility="participants")
        run = self.message_run(self.session)
        execution = IssueExecution.objects.create(issue=issue, issuer=self.user, session=self.session, agent_run=run,
            issueVersion=1, issueTitle=issue.title, issueDescription="")
        delivery = IssueDeliverable.objects.create(issue=issue, execution=execution, agent_run=run,
            author=self.user, recipient=self.other, revision=1, summary="差旅核验结论", artifacts=[])
        self.assertEqual(self.search("核验结论").json()["matches"][0]["id"], "deliverable:" + str(delivery.pk))
        self.client.force_login(self.other)
        self.assertEqual(len(self.search("核验结论").json()["matches"]), 1)
        third = get_user_model().objects.create_user(username="unrelated-searcher")
        WorkspaceMembership.objects.create(workspace=self.workspace, user=third, role="member")
        self.client.force_login(third)
        self.assertEqual(self.search("核验结论").json()["matches"], [])
        self.client.force_login(self.other)
        Issue.objects.filter(pk=issue.pk).update(assignee=None)
        self.assertEqual(self.search("核验结论").json()["matches"], [])

    def test_queries_are_strict_and_original_content_is_integrity_checked(self):
        for query in ("query=", "query=x&query=y", "query=x&unknown=1", "query=x&limit=0", "query=x&cursor=bad"):
            self.assertEqual(self.client.get(self.url + "?" + query).status_code, 400)
        representation = self.index(self.source_object, "line\n" * 230, "sourceObject")
        url = f"{self.url}/materials/sourceObject/{self.source_object.pk}"
        first = self.client.get(url, {"representationId": representation.pk}).json()
        self.assertEqual((first["startLine"], first["endLine"], first["nextOffset"]), (1, 200, 200))
        self.assertEqual(self.client.get(url, {"representationId": representation.pk, "offset": 200}).json()["nextOffset"], None)
        with default_storage.open(representation.canonicalTextKey, "wb") as target:
            target.write(b"changed")
        self.assertEqual(self.client.get(url, {"representationId": representation.pk}).status_code, 409)
