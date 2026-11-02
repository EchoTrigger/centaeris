import base64
import hashlib
import json
from io import BytesIO

from django.contrib.auth.models import User
from django.db import IntegrityError, transaction
from django.db.migrations.executor import MigrationExecutor
from django.test import TestCase, TransactionTestCase
from PIL import Image

from .migration_testing import isolated_migration_database
from .models import Workspace, WorkspaceMembership


def image_data_url(image_format="PNG", size=(80, 40)):
    image = Image.new("RGB", size, "red")
    image.paste("blue", (size[0] // 2, 0, size[0], size[1]))
    image.getexif()[274] = 6
    output = BytesIO()
    image.save(output, format=image_format, exif=image.getexif())
    media_type = {"PNG": "image/png", "JPEG": "image/jpeg", "WEBP": "image/webp", "GIF": "image/gif"}[image_format]
    return f"data:{media_type};base64," + base64.b64encode(output.getvalue()).decode()


class WorkspaceProfileTests(TestCase):
    def setUp(self):
        self.owner = User.objects.create_user("profile-owner")
        self.admin = User.objects.create_user("profile-admin")
        self.member = User.objects.create_user("profile-member")
        self.other = User.objects.create_user("profile-other")
        self.workspace = Workspace.objects.create(name="Original", createdBy=self.owner)
        for user, role in [(self.owner, "owner"), (self.admin, "admin"), (self.member, "member")]:
            WorkspaceMembership.objects.create(workspace=self.workspace, user=user, role=role)
        self.url = f"/api/workspaces/{self.workspace.pk}/profile"

    def patch(self, user, payload):
        self.client.force_login(user)
        return self.client.patch(self.url, data=json.dumps(payload), content_type="application/json")

    def test_owner_and_admin_save_normalized_profile_and_each_supported_format(self):
        for actor, image_format in [(self.owner, "PNG"), (self.admin, "JPEG"), (self.admin, "WEBP")]:
            with self.subTest(image_format=image_format):
                response = self.patch(actor, {"name": "  Renamed  ", "avatarDataUrl": image_data_url(image_format)})
                self.assertEqual(response.status_code, 200, response.content)
                self.workspace.refresh_from_db()
                self.assertEqual(self.workspace.name, "Renamed")
                raw = bytes(self.workspace.avatar_image)
                self.assertEqual(self.workspace.avatar_digest, hashlib.sha256(raw).hexdigest())
                with Image.open(BytesIO(raw)) as image:
                    self.assertEqual((image.format, image.size), ("PNG", (256, 256)))
                    self.assertFalse(image.info)
                    self.assertFalse(image.getexif())
                    # EXIF orientation rotates the two halves from left/right to top/bottom.
                    top, bottom = image.getpixel((128, 32)), image.getpixel((128, 224))
                    self.assertGreater(top[0], top[2])
                    self.assertGreater(bottom[2], bottom[0])
                expected = f"/api/workspaces/{self.workspace.pk}/avatar/{self.workspace.avatar_digest}"
                self.assertEqual(response.json()["workspace"]["avatarUrl"], expected)

    def test_member_and_outsider_cannot_edit(self):
        for actor in (self.member, self.other):
            response = self.patch(actor, {"name": "Unauthorized", "avatarDataUrl": image_data_url()})
            self.assertEqual(response.status_code, 403)
        self.workspace.refresh_from_db()
        self.assertEqual(self.workspace.name, "Original")
        self.assertIsNone(self.workspace.avatar_image)

    def test_avatar_requires_current_membership_and_exact_current_digest(self):
        response = self.patch(self.owner, {"name": "Original", "avatarDataUrl": image_data_url()})
        url = response.json()["workspace"]["avatarUrl"]
        self.client.force_login(self.member)
        response = self.client.get(url)
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response["Content-Type"], "image/png")
        self.assertEqual(response["X-Content-Type-Options"], "nosniff")
        self.assertEqual(response["Cache-Control"], "private, no-store")
        self.assertEqual(self.client.get(url.rsplit("/", 1)[0] + "/" + "0" * 64).status_code, 404)
        WorkspaceMembership.objects.filter(workspace=self.workspace, user=self.member).delete()
        self.assertEqual(self.client.get(url).status_code, 404)
        self.client.force_login(self.other)
        self.assertEqual(self.client.get(url).status_code, 404)

    def test_omitted_avatar_is_preserved_and_null_clears_it(self):
        response = self.patch(self.owner, {"name": "One", "avatarDataUrl": image_data_url()})
        avatar_url = response.json()["workspace"]["avatarUrl"]
        response = self.patch(self.owner, {"name": "Two"})
        self.assertEqual(response.json()["workspace"]["avatarUrl"], avatar_url)
        response = self.patch(self.admin, {"name": "Three", "avatarDataUrl": None})
        self.assertEqual(response.status_code, 200)
        self.assertIsNone(response.json()["workspace"]["avatarUrl"])
        self.workspace.refresh_from_db()
        self.assertEqual((self.workspace.avatar_image, self.workspace.avatar_digest), (None, ""))
        self.assertEqual(self.client.get(avatar_url).status_code, 404)

    def test_invalid_input_leaves_name_and_avatar_unchanged(self):
        self.patch(self.owner, {"name": "Saved", "avatarDataUrl": image_data_url()})
        self.workspace.refresh_from_db()
        saved = (bytes(self.workspace.avatar_image), self.workspace.avatar_digest)
        too_large = "data:image/png;base64," + base64.b64encode(b"x" * (1024 * 1024 + 1)).decode()
        for payload in [
            {},
            {"name": "Changed", "avatarDataUrl": "data:image/png;base64,broken"},
            {"name": "Changed", "avatarDataUrl": "data:image/png;base64," + base64.b64encode(b"not an image").decode()},
            {"name": "Changed", "avatarDataUrl": image_data_url("GIF")},
            {"name": "Changed", "avatarDataUrl": image_data_url().replace("image/png", "image/jpeg")},
            {"name": "Changed", "avatarDataUrl": too_large},
            {"name": "   "}, {"name": "x" * 161}, {"name": 1},
            {"name": "Changed", "avatar_data_url": None},
            {"name": "Changed", "unexpected": True},
        ]:
            with self.subTest(payload_keys=list(payload)):
                response = self.patch(self.owner, payload)
                self.assertEqual(response.status_code, 400, response.content)
                self.workspace.refresh_from_db()
                self.assertEqual(self.workspace.name, "Saved")
                self.assertEqual((bytes(self.workspace.avatar_image), self.workspace.avatar_digest), saved)

    def test_database_rejects_inconsistent_avatar_fields(self):
        for image, digest in [(b"png", ""), (None, "0" * 64), (b"png", "invalid")]:
            with self.subTest(digest=digest), self.assertRaises(IntegrityError), transaction.atomic():
                Workspace.objects.filter(pk=self.workspace.pk).update(avatar_image=image, avatar_digest=digest)


class WorkspaceProfileMigrationTests(TransactionTestCase):
    def test_forward_preserves_existing_name_and_adds_empty_avatar(self):
        old = [("app_core", "0040_question_responses")]
        current = [("app_core", "0041_workspace_avatar")]
        with isolated_migration_database(old) as database:
            before = MigrationExecutor(database).loader.project_state(old).apps
            user = before.get_model("auth", "User").objects.using(database.alias).create(username="profile-migration")
            workspace = before.get_model("app_core", "Workspace").objects.using(database.alias).create(
                name="  Existing 名称  ", createdBy_id=user.pk)
            MigrationExecutor(database).migrate(current)
            after = MigrationExecutor(database).loader.project_state(current).apps
            saved = after.get_model("app_core", "Workspace").objects.using(database.alias).get(pk=workspace.pk)
            self.assertEqual(saved.name, "  Existing 名称  ")
            self.assertEqual((saved.avatar_image, saved.avatar_digest), (None, ""))
