from unittest.mock import patch

from django.contrib.auth import get_user_model
from django.test import TestCase
from django.utils import timezone

from app_core.models import (
    Session,
    Source,
    UserLibraryObject,
    Workspace,
    WorkspaceMembership,
)


class TrashProjectionTests(TestCase):
    def setUp(self):
        User = get_user_model()
        self.owner = User.objects.create_user(username="owner@example.test")
        self.member = User.objects.create_user(username="member@example.test")
        self.workspace = Workspace.objects.create(
            name="Default",
            createdBy=self.owner,
        )
        WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=self.owner,
            role="owner",
        )
        WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=self.member,
            role="member",
        )
        self.session = Session.objects.create(
            workspace=self.workspace,
            owner=self.owner,
            title="Needle Session",
            status="deleted",
            deletedAt=timezone.now(),
            deletedBy=self.owner,
        )
        self.source = Source.objects.create(
            workspace=self.workspace,
            sourceType="fileTree",
            name="Deleted Source",
            status="ready",
            createdBy=self.owner,
        )
        self.folder = UserLibraryObject.objects.create(
            owner=self.owner,
            displayName="Private Folder",
            objectKind="folder",
            contentType="application/vnd.centaeris.folder",
            sizeBytes=0,
            status="ready",
        )
        self.client.force_login(self.owner)
        self.assertEqual(
            self.client.delete(
                f"/api/workspaces/{self.workspace.id}/sources/{self.source.id}"
            ).status_code,
            200,
        )
        self.assertEqual(self.client.delete(f"/api/library/{self.folder.id}").status_code, 200)
        self.files = [
            UserLibraryObject.objects.create(
                owner=self.owner, displayName=name, objectKind=kind, status="deleted",
                deletedAt=timezone.now(), deletedBy=self.owner, deletedFromStatus="ready",
            )
            for name, kind in (("Uploaded file", "file"), ("Uploaded image", "image"), ("Saved output", "savedArtifact"))
        ]
        UserLibraryObject.objects.create(
            owner=self.owner, displayName="Private Note", objectKind="note", status="deleted",
            deletedAt=timezone.now(), deletedBy=self.owner, deletedFromStatus="ready",
        )

    def test_trash_lists_only_library_files_and_binds_cursor(self):
        for item in (self.session, self.source, self.folder):
            item.refresh_from_db()
            self.assertEqual(item.deletedBy_id, self.owner.id)

        response = self.client.get(f"/api/workspaces/{self.workspace.id}/trash")
        self.assertEqual(response.status_code, 200, response.content)
        payload = response.json()
        self.assertEqual({item["id"] for item in payload["items"]}, {item.id for item in self.files})
        self.assertEqual({item["kind"] for item in payload["items"]}, {"library"})
        self.assertTrue(
            all(item["deletedBy"]["userId"] == str(self.owner.id) for item in payload["items"])
        )
        self.assertIn(
            {
                "kind": "libraryRoot",
                "id": None,
                "label": "私人",
                "scope": "privateLibrary",
            },
            payload["filterOptions"]["locations"],
        )

        searched = self.client.get(
            f"/api/workspaces/{self.workspace.id}/trash",
            {"query": "Needle"},
        ).json()
        self.assertEqual(searched["items"], [])
        private = self.client.get(
            f"/api/workspaces/{self.workspace.id}/trash",
            {"scope": "privateLibrary"},
        ).json()
        self.assertEqual({item["id"] for item in private["items"]}, {item.id for item in self.files})
        located = self.client.get(
            f"/api/workspaces/{self.workspace.id}/trash",
            {"locationKind": "workspace"},
        )
        self.assertEqual(located.status_code, 400)

        unsupported_kind = self.client.get(
            f"/api/workspaces/{self.workspace.id}/trash",
            {"kind": "session"},
        )
        self.assertEqual(unsupported_kind.status_code, 400)
        self.assertEqual(unsupported_kind.json(), {"error": "trash_filter_invalid"})

        with (
            patch("app_core.http.trash.TRASH_PAGE_SIZE", 1),
            patch("app_core.http.trash_pagination.TRASH_PAGE_SIZE", 1),
        ):
            first = self.client.get(f"/api/workspaces/{self.workspace.id}/trash").json()
            changed = self.client.get(
                f"/api/workspaces/{self.workspace.id}/trash",
                {"cursor": first["nextCursor"], "query": "Uploaded"},
            )
        self.assertEqual(changed.status_code, 400)
        self.assertEqual(changed.json(), {"error": "trash_cursor_invalid"})
        unknown = self.client.get(
            f"/api/workspaces/{self.workspace.id}/trash",
            {"banana": "1"},
        )
        self.assertEqual(unknown.status_code, 400)
        self.assertEqual(unknown.json(), {"error": "trash_query_invalid"})
        invalid_actor = self.client.get(
            f"/api/workspaces/{self.workspace.id}/trash",
            {"deletedByUserId": "banana"},
        )
        self.assertEqual(invalid_actor.status_code, 400)
        self.assertEqual(invalid_actor.json(), {"error": "trash_filter_invalid"})

        restored = self.client.post(f"/api/sessions/{self.session.id}/restore")
        self.assertEqual(restored.status_code, 200)
        self.session.refresh_from_db()
        self.assertIsNone(self.session.deletedBy_id)

        self.client.force_login(self.member)
        other_view = self.client.get(f"/api/workspaces/{self.workspace.id}/trash").json()
        self.assertEqual(other_view["items"], [])
