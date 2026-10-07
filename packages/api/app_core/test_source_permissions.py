import json
from datetime import timedelta
from unittest.mock import patch

from django.contrib.auth import get_user_model
from django.core.files.uploadedfile import SimpleUploadedFile
from django.test import Client, TestCase
from django.utils import timezone

from app_core.models import (
    Source,
    SourceGrant,
    SourceObject,
    Workspace,
    WorkspaceGroup,
    WorkspaceMembership,
)
from app_core.workspace_access import source_access_is_at_least


User = get_user_model()


class SourcePermissionTests(TestCase):
    def setUp(self):
        self.owner = self.user("owner")
        self.admin = self.user("admin")
        self.member = self.user("member")
        self.controller = self.user("controller")
        self.workspace = Workspace.objects.create(
            name="Default",
            createdBy=self.owner,
        )
        self.owner_membership = self.membership(self.owner, "owner")
        self.admin_membership = self.membership(self.admin, "admin")
        self.member_membership = self.membership(self.member, "member")
        self.controller_membership = self.membership(self.controller, "member")
        self.all_members = self.group("全体成员", kind="all_members")
        self.member_group = self.group("Members")
        self.member_group.members.add(self.member_membership)
        self.controller_group = self.group("Controllers")
        self.controller_group.members.add(self.controller_membership)
        self.source = Source.objects.create(
            workspace=self.workspace,
            sourceType="fileTree",
            name="Research",
            status="ready",
            createdBy=self.owner,
        )
        self.public_object = self.source_object("public/readme.md", "a")
        self.private_object = self.source_object("private/plan.md", "b")

    def user(self, name: str):
        return User.objects.create_user(
            username=f"{name}@example.test",
            password=f"{name}-password",
        )

    def membership(self, user, role: str):
        return WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=user,
            role=role,
        )

    def group(self, name: str, *, kind="custom"):
        return WorkspaceGroup.objects.create(
            workspace=self.workspace,
            name=name,
            kind=kind,
            createdBy=self.owner,
        )

    def source_object(self, display_path: str, digest_character: str):
        return SourceObject.objects.create(
            workspace=self.workspace,
            source=self.source,
            objectType="file",
            displayPath=display_path,
            displayName=display_path.rsplit("/", 1)[-1],
            contentType="text/markdown",
            sizeBytes=4,
            sha256=f"sha256:{digest_character * 64}",
            storageKey=f"sources/{display_path}",
            sourceVersion=f"version-{digest_character}",
            status="ready",
        )

    def request(self, user, method: str, path: str, payload=None):
        self.client.force_login(user)
        callback = getattr(self.client, method)
        if payload is None:
            return callback(path)
        return callback(
            path,
            data=json.dumps(payload),
            content_type="application/json",
        )

    def grant(self, group, access_level: str, source=None):
        return SourceGrant.objects.create(
            workspace=self.workspace,
            source=source or self.source,
            workspaceGroup=group,
            accessLevel=access_level,
            createdBy=self.owner,
        )

    def grant_endpoint(self, grant_id=None):
        endpoint = (
            f"/api/workspaces/{self.workspace.id}/sources/{self.source.id}/grants"
        )
        return f"{endpoint}/{grant_id}" if grant_id else endpoint

    def source_endpoint(self, suffix=""):
        return (
            f"/api/workspaces/{self.workspace.id}/sources/{self.source.id}{suffix}"
        )

    def test_owner_and_admin_have_dynamic_control_without_grants(self):
        for user in (self.owner, self.admin):
            sources = self.request(
                user,
                "get",
                f"/api/workspaces/{self.workspace.id}/sources",
            )
            self.assertEqual(sources.status_code, 200, sources.content)
            self.assertEqual(sources.json()["sources"][0]["accessLevel"], "control")
            objects = self.request(
                user,
                "get",
                f"/api/workspaces/{self.workspace.id}/sources/{self.source.id}/objects",
            )
            self.assertEqual(objects.status_code, 200, objects.content)
            self.assertEqual(len(objects.json()["objects"]), 2)

        member_sources = self.request(
            self.member,
            "get",
            f"/api/workspaces/{self.workspace.id}/sources",
        )
        self.assertEqual(member_sources.status_code, 200)
        self.assertEqual(member_sources.json()["sources"], [])
        hidden = self.request(
            self.member,
            "get",
            f"/api/workspaces/{self.workspace.id}/sources/{self.source.id}/objects",
        )
        self.assertEqual(hidden.status_code, 404)

    def test_read_and_write_apply_to_the_whole_source(self):
        grant = self.grant(self.member_group, "read")
        sources = self.request(
            self.member,
            "get",
            f"/api/workspaces/{self.workspace.id}/sources",
        )
        self.assertEqual(sources.json()["sources"][0]["accessLevel"], "read")
        objects = self.request(
            self.member,
            "get",
            f"/api/workspaces/{self.workspace.id}/sources/{self.source.id}/objects",
        )
        self.assertEqual(len(objects.json()["objects"]), 2)

        upload_path = (
            f"/api/workspaces/{self.workspace.id}/sources/{self.source.id}/objects"
        )
        self.client.force_login(self.member)
        denied = self.client.post(
            upload_path,
            data={
                "displayPath": "public/new.txt",
                "file": SimpleUploadedFile("new.txt", b"new"),
            },
        )
        self.assertEqual(denied.status_code, 404)

        grant.accessLevel = "write"
        grant.save(update_fields=["accessLevel"])
        sources = self.request(
            self.member,
            "get",
            f"/api/workspaces/{self.workspace.id}/sources",
        )
        self.assertEqual(sources.json()["sources"][0]["accessLevel"], "write")
        self.client.force_login(self.member)
        uploaded = self.client.post(
            upload_path,
            data={
                "displayPath": "public/new.txt",
                "file": SimpleUploadedFile("new.txt", b"new"),
            },
        )
        self.assertEqual(uploaded.status_code, 201, uploaded.content)
        second_upload = self.client.post(
            upload_path,
            data={
                "displayPath": "private/new.txt",
                "file": SimpleUploadedFile("new.txt", b"new"),
            },
        )
        self.assertEqual(second_upload.status_code, 201, second_upload.content)
        self.assertEqual(
            self.request(
                self.member,
                "get",
                self.grant_endpoint(),
            ).status_code,
            404,
        )
        self.assertFalse(
            source_access_is_at_least(
                self.member_membership,
                self.source,
                "control",
            )
        )

    def test_root_controller_manages_grants_but_not_workspace_groups_or_sources(self):
        controller_grant = self.grant(self.controller_group, "control")
        listed = self.request(
            self.controller,
            "get",
            self.grant_endpoint(),
        )
        self.assertEqual(listed.status_code, 200, listed.content)
        self.assertEqual(listed.json()["grants"][0]["accessLevel"], "control")

        created = self.request(
            self.controller,
            "post",
            self.grant_endpoint(),
            {
                "workspaceGroupId": self.member_group.id,
                "accessLevel": "write",
            },
        )
        self.assertEqual(created.status_code, 201, created.content)
        delegated_id = created.json()["grant"]["id"]
        updated = self.request(
            self.controller,
            "patch",
            self.grant_endpoint(delegated_id),
            {"accessLevel": "read"},
        )
        self.assertEqual(updated.status_code, 200, updated.content)
        self.assertEqual(updated.json()["grant"]["accessLevel"], "read")
        self.assertEqual(
            self.request(
                self.controller,
                "delete",
                self.grant_endpoint(delegated_id),
            ).status_code,
            200,
        )

        group_create = self.request(
            self.controller,
            "post",
            f"/api/workspaces/{self.workspace.id}/groups",
            {"name": "Not allowed"},
        )
        self.assertEqual(group_create.status_code, 404)
        source_create = self.request(
            self.controller,
            "post",
            f"/api/workspaces/{self.workspace.id}/sources",
            {"sourceType": "fileTree", "name": "Not allowed"},
        )
        self.assertEqual(source_create.status_code, 404)

        self.assertEqual(
            self.request(
                self.controller,
                "delete",
                self.grant_endpoint(controller_grant.id),
            ).status_code,
            200,
        )
        self.assertEqual(
            self.request(
                self.controller,
                "get",
                self.grant_endpoint(),
            ).status_code,
            404,
        )

    def test_controller_can_delegate_root_control_to_an_existing_group(self):
        self.grant(self.controller_group, "control")
        delegated = self.request(
            self.controller,
            "post",
            self.grant_endpoint(),
            {
                "workspaceGroupId": self.all_members.id,
                "accessLevel": "control",
            },
        )
        self.assertEqual(delegated.status_code, 201, delegated.content)
        self.assertTrue(
            source_access_is_at_least(
                self.member_membership,
                self.source,
                "control",
            )
        )

    def test_unknown_access_loud_fails(self):
        invalid = self.request(
            self.owner,
            "post",
            self.grant_endpoint(),
            {
                "workspaceGroupId": self.member_group.id,
                "accessLevel": "banana",
            },
        )
        self.assertEqual(invalid.status_code, 400)
        self.assertEqual(invalid.json()["error"], "invalid_source_grant")
        with self.assertRaisesRegex(
            ValueError,
            "unsupported SourceGrant.accessLevel: banana",
        ):
            self.grant(self.member_group, "banana")

    def test_grant_identity_conflicts_and_mutations_are_transactional(self):
        grant = self.grant(self.member_group, "read")
        duplicate = self.request(
            self.owner,
            "post",
            self.grant_endpoint(),
            {
                "workspaceGroupId": self.member_group.id,
                "accessLevel": "write",
            },
        )
        self.assertEqual(duplicate.status_code, 409)
        unchanged = self.request(
            self.owner,
            "patch",
            self.grant_endpoint(grant.id),
            {"accessLevel": "read"},
        )
        self.assertEqual(unchanged.status_code, 409)

        with patch.object(
            SourceGrant,
            "delete",
            side_effect=RuntimeError("grant delete failed"),
        ):
            failed = self.request(
                self.owner,
                "delete",
                self.grant_endpoint(grant.id),
            )
        self.assertEqual(failed.status_code, 500)
        self.assertTrue(SourceGrant.objects.filter(id=grant.id).exists())

        other = self.user("other")
        other_workspace = Workspace.objects.create(name="Other", createdBy=other)
        other_group = WorkspaceGroup.objects.create(
            workspace=other_workspace,
            name="Other group",
            createdBy=other,
        )
        cross_workspace = self.request(
            self.owner,
            "post",
            self.grant_endpoint(),
            {
                "workspaceGroupId": other_group.id,
                "accessLevel": "read",
            },
        )
        self.assertEqual(cross_workspace.status_code, 400)

    def test_controller_can_rename_a_source(self):
        self.grant(self.controller_group, "control")
        self.grant(self.member_group, "read")

        renamed = self.request(
            self.controller,
            "patch",
            self.source_endpoint(),
            {"name": "  Cafe\u0301  "},
        )
        self.assertEqual(renamed.status_code, 200, renamed.content)
        self.assertEqual(renamed.json()["source"]["name"], "Café")
        self.assertEqual(
            self.request(
                self.controller,
                "patch",
                self.source_endpoint(),
                {"name": "Café"},
            ).status_code,
            409,
        )
        invalid = self.request(
            self.controller,
            "patch",
            self.source_endpoint(),
            {"name": "   "},
        )
        self.assertEqual(invalid.status_code, 400)
        self.assertEqual(invalid.json(), {"error": "invalid_source"})

    def test_failed_source_restores_its_exact_previous_status(self):
        self.source.status = "failed"
        self.source.failureReason = "index_failed"
        self.source.save(update_fields=["status", "failureReason", "updatedAt"])

        self.assertEqual(
            self.request(
                self.owner,
                "delete",
                self.source_endpoint(),
            ).status_code,
            200,
        )
        self.assertEqual(
            self.request(
                self.owner,
                "post",
                self.source_endpoint("/restore"),
            ).status_code,
            200,
        )
        self.source.refresh_from_db()
        self.assertEqual(self.source.status, "failed")
        self.assertEqual(self.source.failureReason, "index_failed")

    def test_delete_and_restore_preserve_source_identity_history_and_grants(self):
        grant = self.grant(self.controller_group, "control")
        object_ids = set(self.source.sourceObjects.values_list("id", flat=True))

        deleted = self.request(
            self.controller,
            "delete",
            self.source_endpoint(),
        )

        self.assertEqual(deleted.status_code, 200, deleted.content)
        self.source.refresh_from_db()
        self.assertEqual(self.source.status, "deleted")
        self.assertEqual(self.source.deletedFromStatus, "ready")
        self.assertIsNotNone(self.source.deletedAt)
        self.assertEqual(
            set(self.source.sourceObjects.values_list("id", flat=True)),
            object_ids,
        )
        self.assertTrue(SourceGrant.objects.filter(id=grant.id).exists())
        self.assertEqual(
            self.request(
                self.controller,
                "get",
                f"/api/workspaces/{self.workspace.id}/sources",
            ).json()["sources"],
            [],
        )
        trash = self.request(
            self.controller,
            "get",
            f"/api/workspaces/{self.workspace.id}/trash?kind=source",
        )
        self.assertEqual(trash.status_code, 200, trash.content)
        self.assertEqual(
            [source["id"] for source in trash.json()["items"]],
            [self.source.id],
        )
        self.assertIsNotNone(trash.json()["items"][0]["deletedAt"])
        for method, suffix, payload in (
            ("delete", "", None),
            ("patch", "", {"name": "New name"}),
            ("get", "/objects", None),
            ("get", "/grants", None),
        ):
            response = self.request(
                self.controller,
                method,
                self.source_endpoint(suffix),
                payload,
            )
            self.assertEqual(response.status_code, 410, response.content)
            self.assertEqual(response.json(), {"error": "source_deleted"})

        with patch.object(Source, "save", side_effect=RuntimeError("restore failed")):
            failed = self.request(
                self.controller,
                "post",
                self.source_endpoint("/restore"),
            )
        self.assertEqual(failed.status_code, 500)
        self.source.refresh_from_db()
        self.assertEqual(self.source.status, "deleted")
        self.assertEqual(self.source.deletedFromStatus, "ready")

        restored = self.request(
            self.controller,
            "post",
            self.source_endpoint("/restore"),
        )
        self.assertEqual(restored.status_code, 200, restored.content)
        self.assertEqual(restored.json()["source"]["id"], self.source.id)
        self.assertEqual(restored.json()["source"]["status"], "ready")
        self.source.refresh_from_db()
        self.assertEqual(self.source.deletedFromStatus, "")
        self.assertIsNone(self.source.deletedAt)
        self.assertTrue(SourceGrant.objects.filter(id=grant.id).exists())
        self.assertEqual(
            set(self.source.sourceObjects.values_list("id", flat=True)),
            object_ids,
        )
        self.assertEqual(
            self.request(
                self.controller,
                "get",
                f"/api/workspaces/{self.workspace.id}/trash?kind=source",
            ).json()["items"],
            [],
        )
        repeated = self.request(
            self.controller,
            "post",
            self.source_endpoint("/restore"),
        )
        self.assertEqual(repeated.status_code, 409)
        self.assertEqual(repeated.json(), {"error": "source_not_restorable"})

    def test_source_trash_expires_and_supports_permanent_delete(self):
        self.grant(self.controller_group, "control")
        self.assertEqual(
            self.request(self.controller, "delete", self.source_endpoint()).status_code,
            200,
        )
        Source.objects.filter(id=self.source.id).update(
            deletedAt=timezone.now() - timedelta(days=31),
        )
        trash = self.request(
            self.controller,
            "get",
            f"/api/workspaces/{self.workspace.id}/trash?kind=source",
        )
        self.assertEqual(trash.json()["items"], [])
        expired = self.request(
            self.controller,
            "post",
            self.source_endpoint("/restore"),
        )
        self.assertEqual(expired.status_code, 410)
        self.assertEqual(expired.json(), {"error": "source_expired"})
        permanent = self.request(
            self.controller,
            "delete",
            self.source_endpoint("/trash"),
        )
        self.assertEqual(permanent.status_code, 200)
        self.source.refresh_from_db()
        self.assertIsNotNone(self.source.purgedAt)

    def test_source_trash_uses_control_acl_and_fixed_cursor_pages(self):
        deleted_sources = [
            Source(
                workspace=self.workspace,
                sourceType="fileTree",
                name=f"Deleted {index:02d}",
                status="deleted",
                deletedAt=timezone.now() - timedelta(seconds=index),
                deletedFromStatus="ready",
                createdBy=self.owner,
            )
            for index in range(51)
        ]
        Source.objects.bulk_create(deleted_sources)
        controller_grant = self.grant(
            self.controller_group,
            "control",
            source=deleted_sources[0],
        )

        controller_trash = self.request(
            self.controller,
            "get",
            f"/api/workspaces/{self.workspace.id}/trash?kind=source",
        ).json()
        self.assertEqual(
            [source["id"] for source in controller_trash["items"]],
            [deleted_sources[0].id],
        )
        controller_grant.delete()
        self.assertEqual(
            self.request(
                self.controller,
                "get",
                f"/api/workspaces/{self.workspace.id}/trash?kind=source",
            ).json()["items"],
            [],
        )
        self.assertEqual(
            self.request(
                self.controller,
                "post",
                f"/api/workspaces/{self.workspace.id}/sources/{deleted_sources[0].id}/restore",
            ).status_code,
            404,
        )
        self.assertEqual(
            self.request(
                self.member,
                "get",
                f"/api/workspaces/{self.workspace.id}/trash?kind=source",
            ).json()["items"],
            [],
        )

        first = self.request(
            self.owner,
            "get",
            f"/api/workspaces/{self.workspace.id}/trash?kind=source",
        ).json()
        second = self.client.get(
            f"/api/workspaces/{self.workspace.id}/trash",
            {"cursor": first["nextCursor"], "kind": "source"},
        ).json()
        self.assertEqual(len(first["items"]), 50)
        self.assertTrue(first["hasMore"])
        self.assertEqual(len(second["items"]), 1)
        self.assertFalse(second["hasMore"])
        invalid = self.client.get(
            f"/api/workspaces/{self.workspace.id}/trash",
            {"cursor": "banana", "kind": "source"},
        )
        self.assertEqual(invalid.status_code, 400)
        self.assertEqual(invalid.json(), {"error": "trash_cursor_invalid"})
        cross_endpoint = self.client.get(
            f"/api/workspaces/{self.workspace.id}/trash",
            {"cursor": first["nextCursor"], "kind": "agent"},
        )
        self.assertEqual(cross_endpoint.status_code, 400)
        self.assertEqual(cross_endpoint.json(), {"error": "trash_cursor_invalid"})

    def test_lifecycle_requires_control_and_failed_mutation_rolls_back(self):
        self.grant(self.member_group, "write")
        for method, suffix, payload in (
            ("patch", "", {"name": "Denied"}),
            ("delete", "", None),
        ):
            self.assertEqual(
                self.request(
                    self.member,
                    method,
                    self.source_endpoint(suffix),
                    payload,
                ).status_code,
                404,
            )

        with patch.object(Source, "save", side_effect=RuntimeError("delete failed")):
            failed = self.request(
                self.owner,
                "delete",
                self.source_endpoint(),
            )
        self.assertEqual(failed.status_code, 500)
        self.source.refresh_from_db()
        self.assertEqual(self.source.status, "ready")

        self.source.status = "archived"
        with self.assertRaisesRegex(ValueError, "unsupported Source.status: archived"):
            self.source.save()
        self.source.status = "deleted"
        self.source.deletedAt = timezone.now()
        self.source.deletedFromStatus = "banana"
        with self.assertRaisesRegex(ValueError, "Source lifecycle shape is invalid"):
            self.source.save()

    def test_source_lifecycle_mutations_require_csrf(self):
        client = Client(enforce_csrf_checks=True)
        client.force_login(self.owner)
        requests = (
            client.patch(
                self.source_endpoint(),
                data=json.dumps({"name": "Denied"}),
                content_type="application/json",
            ),
            client.post(self.source_endpoint("/restore")),
            client.delete(self.source_endpoint()),
        )
        for response in requests:
            self.assertEqual(response.status_code, 403)
            self.assertEqual(response.json(), {"error": "csrf_failed"})
        self.source.refresh_from_db()
        self.assertEqual(self.source.status, "ready")
