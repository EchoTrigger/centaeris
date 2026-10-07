"""Pagination must not depend on the database's default string collation."""

from datetime import timedelta

from django.contrib.auth import get_user_model
from django.test import TestCase
from django.utils import timezone

from .models import Agent, Session, Source, UserLibraryObject, Workspace, WorkspaceMembership


class TrashOrderingTests(TestCase):
    @classmethod
    def setUpTestData(cls):
        cls.user = get_user_model().objects.create_user(username="trash-order")
        cls.workspace = Workspace.objects.create(name="Trash order", createdBy=cls.user)
        WorkspaceMembership.objects.create(workspace=cls.workspace, user=cls.user, role="owner")
        parent = Agent.objects.create(workspace=cls.workspace, owner=cls.user, name="Active parent")
        instant = timezone.now() - timedelta(minutes=1)
        cls.expected = []
        # Identical timestamps force ID comparisons at multiple page boundaries.
        identities = [(f"{letter}{number:03}", instant) for letter in "aA_0-Zz" for number in range(17)]
        # Deliberately oppose lexical order: time must remain the primary key.
        identities += [("zz-newest", instant + timedelta(seconds=1)), ("00-oldest", instant - timedelta(seconds=1))]
        for kind in ("agent", "library", "session", "source"):
            for suffix, deleted_at in identities:
                identity = f"{kind}_{suffix}"
                base = {"id": identity, "status": "deleted", "deletedAt": deleted_at}
                if kind == "agent":
                    Agent.objects.create(**base, workspace=cls.workspace, owner=cls.user, name=identity)
                elif kind == "session":
                    Session.objects.create(**base, workspace=cls.workspace, owner=cls.user, agent=parent)
                elif kind == "source":
                    Source.objects.create(**base, workspace=cls.workspace, createdBy=cls.user,
                                          name=identity, sourceType="uploadedFile", deletedFromStatus="ready")
                else:
                    UserLibraryObject.objects.create(**base, owner=cls.user, displayName=identity,
                                                     objectKind="folder", deletedFromStatus="ready")
                cls.expected.append((deleted_at, kind, identity))
        cls.expected.sort(key=lambda item: (item[1], item[2]))
        cls.expected.sort(key=lambda item: item[0], reverse=True)

    def assert_pages(self, kind=None):
        self.client.force_login(self.user)
        expected = [(item_kind, identity) for _, item_kind, identity in self.expected
                    if kind is None or item_kind == kind]
        collected, cursors = [], set()
        query = {"kind": kind} if kind else {}
        for offset in range(0, len(expected), 50):
            response = self.client.get(f"/api/workspaces/{self.workspace.id}/trash", query)
            self.assertEqual(response.status_code, 200, response.content)
            body = response.json()
            page = [(item["kind"], item["id"]) for item in body["items"]]
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

    def test_each_kind_has_exact_pages_and_no_missing_or_repeated_ids(self):
        for kind in ("agent", "library", "session", "source"):
            with self.subTest(kind=kind):
                self.assert_pages(kind)

    def test_mixed_kinds_share_the_same_total_order_and_cursor(self):
        self.assert_pages()
