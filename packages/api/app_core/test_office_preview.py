from unittest.mock import AsyncMock, patch

from asgiref.sync import async_to_sync
from django.contrib.auth.models import User
from django.test import TestCase

from .http import storage_stream
from .material_identity import processing_spec_digest, representation_id
from .models import (
    DerivedRepresentation,
    MaterialProcessingTask,
    MaterialProcessor,
    ProcessingSpecification,
    Source,
    SourceObject,
    UserLibraryObject,
    Workspace,
    WorkspaceMembership,
)


class OfficePreviewTests(TestCase):
    def setUp(self):
        self.user = User.objects.create_user(username="office-reader")
        self.item = UserLibraryObject.objects.create(
            owner=self.user,
            objectKind="file",
            status="ready",
            displayName="report.docx",
            contentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            storageKey="report.docx",
            sizeBytes=4,
            sha256="sha256:" + "a" * 64,
            contentGeneration=1,
        )
        self.specification = {
            "schema": "knowledge.processing_specification.v1",
            "processorId": "centaeris.document.cpu",
            "processorVersion": "1.0.0",
            "executionImageDigest": "sha256:" + "b" * 64,
            "modelDigests": {
                "PP-OCRv6_small_det": "sha256:" + "c" * 64,
                "PP-OCRv6_small_rec": "sha256:" + "d" * 64,
            },
            "options": {
                "renderDpi": 220,
                "maxInputBytes": 64 * 1024 * 1024,
                "maxRenderedPixelsPerPage": 16_000_000,
                "maxOutputBytes": 256 * 1024 * 1024,
            },
        }
        self.spec_digest = processing_spec_digest(self.specification)
        specification = ProcessingSpecification.objects.create(
            specDigest=self.spec_digest,
            payload=self.specification,
        )
        MaterialProcessor.objects.create(name="document", specification=specification)
        self.url = f"/api/office-preview/userLibraryObject/{self.item.pk}"
        self.client.force_login(self.user)

    def identity(self, item=None, kind="userLibraryObject"):
        item = item or self.item
        return {
            "ownerKind": kind,
            "ownerId": item.pk,
            "generation": item.contentGeneration,
            "sha256": item.sha256,
        }

    def representation(self, item=None, kind="userLibraryObject"):
        return representation_id(self.identity(item, kind), self.spec_digest)

    def publish_preview(self, item=None, kind="userLibraryObject", workbook=False):
        item = item or self.item
        return DerivedRepresentation.objects.create(
            representationId=self.representation(item, kind),
            ownerKind=kind,
            ownerId=item.pk,
            ownerContentGeneration=item.contentGeneration,
            ownerSha256=item.sha256,
            processingSpecification_id=self.spec_digest,
            pageCount=1,
            canonicalTextKey="knowledge/report/canonical.md",
            canonicalTextSizeBytes=8,
            canonicalTextSha256="sha256:" + "e" * 64,
            previewPdfKey="knowledge/report/preview.pdf",
            previewPdfSizeBytes=8,
            previewPdfSha256="sha256:" + "f" * 64,
            workbookPreviewKey="knowledge/report/workbook.json" if workbook else "",
            workbookPreviewSizeBytes=10 if workbook else 0,
            workbookPreviewSha256="sha256:" + "1" * 64 if workbook else "",
            manifest={"schema": "knowledge.derived_manifest.v1", "pages": [], "pageCount": 1},
        )

    def test_xlsx_table_preview_streams_the_inert_workbook_model(self):
        self.item.displayName = "预算.xlsx"
        self.item.contentType = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        self.item.save(update_fields=["displayName", "contentType"])
        self.publish_preview(workbook=True)

        async def chunks():
            yield b'{"schema":1}'

        with patch.object(storage_stream, "open_storage_stream", new=AsyncMock(return_value=chunks())):
            response = self.client.get(f"/api/spreadsheet-preview/userLibraryObject/{self.item.pk}")
            self.assertEqual(response.status_code, 200)
            self.assertEqual(response["Content-Type"], "application/json")
            self.assertEqual(response["Content-Length"], "10")
            self.assertTrue(response["Content-Disposition"].startswith("inline;"))

    @patch("app_core.material_task_source.default_storage.exists", return_value=True)
    def test_missing_preview_enqueues_exact_file_version_and_returns_local_loading_page(self, _exists):
        response = self.client.get(self.url)
        self.assertEqual(response.status_code, 202)
        self.assertEqual(response["Retry-After"], "1")
        self.assertEqual(response["Cache-Control"], "no-store")
        self.assertContains(response, "正在生成只读预览", status_code=202)
        self.assertContains(
            response,
            f'http-equiv="refresh" content="1;url={self.url}?lang=zh-CN"',
            status_code=202,
        )
        self.assertEqual(response["Refresh"], f"1;url={self.url}?lang=zh-CN")
        body = response.content.decode()
        self.assertNotIn("Collabora", body)
        self.assertNotIn("access_token", body)
        self.assertNotIn("<iframe", body)
        task = MaterialProcessingTask.objects.get(pk=self.representation())
        self.assertEqual(task.status, "pending")
        self.assertEqual(task.processingSpecification, self.specification)
        self.assertEqual(task.payload, {
            "schema": "workspace.material.processing.v1",
            "inputIdentity": self.identity(),
            "specDigest": self.spec_digest,
            "sizeBytes": 4,
        })

    def test_completed_preview_streams_only_the_derived_pdf_inline(self):
        self.publish_preview()

        async def chunks():
            yield b"%PDF-1.7"

        with patch.object(storage_stream, "open_storage_stream", new=AsyncMock(return_value=chunks())):
            response = self.client.get(self.url)
            self.assertEqual(response.status_code, 200)
            self.assertEqual(response["Content-Type"], "application/pdf")
            self.assertEqual(response["Content-Length"], "8")
            self.assertTrue(response["Content-Disposition"].startswith("inline;"))

            async def read():
                return b"".join([part async for part in response.streaming_content])

            self.assertEqual(async_to_sync(read)(), b"%PDF-1.7")
        self.assertFalse(MaterialProcessingTask.objects.exists())

    @patch("app_core.material_task_source.default_storage.exists", return_value=True)
    def test_changed_generation_never_reuses_a_stale_preview(self, _exists):
        self.publish_preview()
        UserLibraryObject.objects.filter(pk=self.item.pk).update(
            contentGeneration=2,
            sha256="sha256:" + "1" * 64,
        )
        self.item.refresh_from_db()
        response = self.client.get(self.url)
        self.assertEqual(response.status_code, 202)
        self.assertTrue(MaterialProcessingTask.objects.filter(pk=self.representation()).exists())

    @patch("app_core.material_task_source.default_storage.exists", return_value=True)
    def test_completed_task_without_a_published_preview_fails_loudly(self, _exists):
        response = self.client.get(self.url)
        self.assertEqual(response.status_code, 202)
        MaterialProcessingTask.objects.filter(pk=self.representation()).update(status="completed")
        response = self.client.get(self.url)
        self.assertEqual(response.status_code, 503)
        self.assertEqual(response.json(), {"error": "office_preview_result_missing"})

    def test_missing_document_processor_fails_loudly(self):
        MaterialProcessor.objects.all().delete()
        response = self.client.get(self.url)
        self.assertEqual(response.status_code, 503)
        self.assertEqual(response.json(), {"error": "office_preview_processor_unavailable"})

    def test_other_user_cannot_open_preview_and_unsupported_files_fail(self):
        other = User.objects.create_user(username="office-other")
        self.client.force_login(other)
        self.assertEqual(self.client.get(self.url).status_code, 404)
        self.client.force_login(self.user)
        UserLibraryObject.objects.filter(pk=self.item.pk).update(displayName="script.html", contentType="text/html")
        self.assertEqual(self.client.get(self.url).status_code, 415)

    @patch("app_core.material_task_source.default_storage.exists", return_value=True)
    def test_source_preview_rechecks_workspace_membership(self, _exists):
        workspace = Workspace.objects.create(name="Office", createdBy=self.user)
        membership = WorkspaceMembership.objects.create(workspace=workspace, user=self.user, role="owner")
        source = Source.objects.create(workspace=workspace, createdBy=self.user, sourceType="fileTree", name="Files", status="ready")
        item = SourceObject.objects.create(
            workspace=workspace,
            source=source,
            objectType="file",
            displayName="report.docx",
            displayPath="report.docx",
            storageKey="report.docx",
            sizeBytes=4,
            sha256="sha256:" + "a" * 64,
            sourceVersion="v1",
            status="ready",
        )
        url = f"/api/office-preview/sourceObject/{item.pk}"
        self.assertEqual(self.client.get(url).status_code, 202)
        membership.delete()
        self.assertEqual(self.client.get(url).status_code, 404)

    def test_utf8_text_preview_declares_encoding_without_changing_bytes(self):
        async def chunks():
            yield "中文预览".encode()

        async def check():
            with patch.object(storage_stream, "open_storage_stream", new=AsyncMock(return_value=chunks())):
                response = await storage_stream.stored_file_response(
                    "file.md", "text/markdown", "file.md", as_attachment=False
                )
                self.assertEqual(response["Content-Type"], "text/markdown; charset=utf-8")
                self.assertEqual(
                    b"".join([part async for part in response.streaming_content]),
                    "中文预览".encode(),
                )

        async_to_sync(check)()
