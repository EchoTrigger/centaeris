"""Pagination must not depend on the database's default string collation."""

from datetime import timedelta

from django.contrib.auth import get_user_model
from django.test import TestCase
from django.utils import timezone

from .models import UserLibraryObject, Workspace, WorkspaceMembership


class TrashOrderingTests(TestCase):
    @classmethod
    def setUpTestData(cls):
        cls.user = get_user_model().objects.create_user(username="trash-order")
        cls.workspace = Workspace.objects.create(name="Trash order", createdBy=cls.user)
        WorkspaceMembership.objects.create(workspace=cls.workspace, user=cls.user, role="owner")
        instant = timezone.now() - timedelta(minutes=1)
        cls.expected = []
        # Identical timestamps force ID comparisons at multiple page boundaries.
        identities = [(f"{letter}{number:03}", instant) for letter in "aA_0-Zz" for number in range(17)]
        # Deliberately oppose lexical order: time must remain the primary key.
        identities += [("zz-newest", instant + timedelta(seconds=1)), ("00-oldest", instant - timedelta(seconds=1))]
        for suffix, deleted_at in identities:
            identity = f"library_{suffix}"
            UserLibraryObject.objects.create(id=identity, status="deleted", deletedAt=deleted_at,
                                             owner=cls.user, displayName=identity, objectKind="file",
                                             deletedFromStatus="ready")
            cls.expected.append((deleted_at, identity))
        cls.expected.sort(key=lambda item: item[1])
        cls.expected.sort(key=lambda item: item[0], reverse=True)

    def test_files_have_exact_pages_with_no_missing_or_repeated_ids(self):
        self.client.force_login(self.user)
        expected = [identity for _, identity in self.expected]
        collected, cursors = [], set()
        query = {}
        for offset in range(0, len(expected), 50):
            response = self.client.get(f"/api/workspaces/{self.workspace.id}/trash", query)
            self.assertEqual(response.status_code, 200, response.content)
            body = response.json()
            page = [item["id"] for item in body["items"]]
            self.assertEqual(page, expected[offset:offset + 50])
            collected.extend(page)
            has_more = offset + 50 < len(expected)
            self.assertEqual(body["hasMore"], has_more)
            if has_more:
                self.assertIsInstance(body["nextCursor"], str)
                self.assertNotIn(body["nextCursor"], cursors)
                cursors.add(body["nextCursor"])
                query["cursor"] = body["nextCursor"]
            else:
                self.assertIsNone(body["nextCursor"])
        self.assertEqual(collected, expected)
        self.assertEqual(len(set(collected)), len(expected))
