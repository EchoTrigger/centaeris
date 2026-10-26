"""Private custom avatars share the Agent profile's atomic save and access boundary."""
import base64
import hashlib
import json
from datetime import timedelta
from io import BytesIO

from django.contrib.auth import get_user_model
from django.test import Client, TestCase
from django.utils import timezone
from PIL import Image, PngImagePlugin

from .models import Agent, AgentDefinition, AgentDefinitionVersion, Workspace, WorkspaceMembership


def image_data(format="PNG", *, size=(80, 40), color="red", animated=False, metadata=False):
    image = Image.new("RGB", size, color)
    stream = BytesIO()
    options = {}
    if animated:
        options.update(save_all=True, append_images=[Image.new("RGB", size, "blue")], duration=100, loop=0)
    if metadata and format == "PNG":
        info = PngImagePlugin.PngInfo(); info.add_text("private-note", "must not survive")
        options["pnginfo"] = info
    image.save(stream, format=format, **options)
    return base64.b64encode(stream.getvalue()).decode("ascii")


class AgentAvatarTests(TestCase):
    def setUp(self):
        User = get_user_model()
        self.user = User.objects.create_user(username="avatar-owner")
        self.other = User.objects.create_user(username="avatar-other")
        self.operator = User.objects.create_superuser(username="avatar-operator", password="test")
        self.workspace = Workspace.objects.create(name="Avatars", createdBy=self.user)
        self.membership = WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="member")
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.other, role="admin")
        self.client.force_login(self.user)
        self.base = f"/api/workspaces/{self.workspace.id}/agents"

    def send(self, method, path, value):
        return getattr(self.client, method)(path, json.dumps(value), content_type="application/json")

    def create(self, **fields):
        response = self.send("post", self.base, {"name": "Private agent", **fields})
        self.assertEqual(response.status_code, 201, response.content)
        return response.json()["agent"]

    def test_create_upload_is_normalized_private_png_without_embedded_metadata(self):
        encoded = image_data(metadata=True)
        data = self.create(avatarDataBase64=encoded)
        self.assertEqual(data["avatarKind"], "centaeris")
        self.assertRegex(data["avatarUrl"], rf"^/api/agents/{data['id']}/avatar\?v=[0-9a-f]{{64}}$")
        self.assertNotIn("avatarDataBase64", data)
        self.assertNotIn("avatarImage", data)
        response = self.client.get(data["avatarUrl"])
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response["Content-Type"], "image/png")
        self.assertEqual(response["Cache-Control"], "private, no-store")
        self.assertEqual(response["X-Content-Type-Options"], "nosniff")
        self.assertLessEqual(len(response.content), 512 * 1024)
        with Image.open(BytesIO(response.content)) as avatar:
            self.assertEqual(avatar.size, (256, 256))
            self.assertEqual(avatar.format, "PNG")
            self.assertNotIn("private-note", avatar.info)
        agent = Agent.objects.get(pk=data["id"])
        self.assertEqual(bytes(agent.avatar_image), response.content)
        self.assertEqual(agent.avatar_digest, hashlib.sha256(response.content).hexdigest())

    def test_update_rename_preserves_avatar_replace_versions_it_and_reset_restores_builtin(self):
        data = self.create(avatarDataBase64=image_data())
        path = f"/api/agents/{data['id']}"
        renamed = self.send("patch", path, {"name": "Renamed"})
        self.assertEqual(renamed.status_code, 200, renamed.content)
        self.assertEqual(renamed.json()["agent"]["avatarUrl"], data["avatarUrl"])
        replaced = self.send("patch", path, {"avatarDataBase64": image_data(color="blue")})
        self.assertEqual(replaced.status_code, 200, replaced.content)
        self.assertNotEqual(replaced.json()["agent"]["avatarUrl"], data["avatarUrl"])
        self.assertEqual(self.client.get(data["avatarUrl"]).status_code, 404)
        reset = self.send("patch", path, {"avatarDataBase64": None, "avatarKind": "banana"})
        self.assertEqual(reset.status_code, 200, reset.content)
        self.assertIsNone(reset.json()["agent"]["avatarUrl"])
        self.assertEqual(reset.json()["agent"]["avatarKind"], "banana")
        agent = Agent.objects.get(pk=data["id"])
        self.assertIsNone(agent.avatar_image)
        self.assertEqual(agent.avatar_digest, "")
        self.assertEqual(self.client.get(replaced.json()["agent"]["avatarUrl"]).status_code, 404)

    def test_avatar_read_and_write_require_current_owner_and_workspace_membership(self):
        data = self.create(avatarDataBase64=image_data())
        path = f"/api/agents/{data['id']}"
        for user in (self.other, self.operator):
            self.client.force_login(user)
            self.assertEqual(self.client.get(data["avatarUrl"]).status_code, 404)
            self.assertEqual(self.send("patch", path, {"avatarDataBase64": None}).status_code, 404)
        foreign = Workspace.objects.create(name="Elsewhere", createdBy=self.user)
        WorkspaceMembership.objects.create(workspace=foreign, user=self.user, role="admin")
        self.membership.delete()
        self.client.force_login(self.user)
        self.assertEqual(self.client.get(data["avatarUrl"]).status_code, 404)
        self.assertEqual(self.send("patch", path, {"avatarDataBase64": image_data()}).status_code, 404)
        self.assertEqual(Client().get(data["avatarUrl"]).status_code, 401)

    def test_invalid_avatar_keeps_entire_profile_unchanged_and_creates_nothing(self):
        original = self.create(avatarDataBase64=image_data())
        agent = Agent.objects.get(pk=original["id"])
        before = (agent.name, agent.description, agent.instructions, agent.avatar_digest, bytes(agent.avatar_image))
        corrupted = bytearray(base64.b64decode(image_data()))
        idat = corrupted.index(b"IDAT")
        checksum = idat + 4 + int.from_bytes(corrupted[idat - 4:idat], "big")
        corrupted[checksum] ^= 1
        samples = {
            "invalid-base64": "not base64!",
            "empty": "",
            "corrupt-checksum": base64.b64encode(corrupted).decode(),
            "non-image": base64.b64encode(b"not an image").decode(),
            "svg": base64.b64encode(b'<svg xmlns="http://www.w3.org/2000/svg"/>').decode(),
            "gif": image_data("GIF"),
            "animated-png": image_data(animated=True),
            "truncated": base64.b64encode(base64.b64decode(image_data())[:45]).decode(),
            "too-many-pixels": image_data(size=(4097, 4097)),
            "too-many-bytes": base64.b64encode(b"x" * (1024 * 1024 + 1)).decode(),
        }
        for label, encoded in samples.items():
            with self.subTest(label=label):
                response = self.send("patch", f"/api/agents/{agent.pk}", {"name": "Must not persist", "avatarDataBase64": encoded})
                self.assertEqual(response.status_code, 400, response.content)
                self.assertIn(response.json()["error"], {"agent_avatar_invalid", "agent_avatar_too_large", "agent_avatar_dimensions_invalid"})
                agent.refresh_from_db()
                self.assertEqual((agent.name, agent.description, agent.instructions, agent.avatar_digest, bytes(agent.avatar_image)), before)
                created = self.send("post", self.base, {"name": "Must not exist", "avatarDataBase64": encoded})
                self.assertEqual(created.status_code, 400, created.content)
                self.assertEqual(Agent.objects.count(), 1)

    def test_jpeg_and_webp_uploads_are_accepted_but_do_not_preserve_original_format(self):
        for format in ("JPEG", "WEBP"):
            with self.subTest(format=format):
                data = self.create(avatarDataBase64=image_data(format))
                response = self.client.get(data["avatarUrl"])
                self.assertEqual(response.status_code, 200)
                with Image.open(BytesIO(response.content)) as image:
                    self.assertEqual((image.format, image.size), ("PNG", (256, 256)))

    def test_exif_orientation_is_applied_and_private_camera_metadata_is_removed(self):
        image = Image.new("RGB", (80, 40), "red")
        image.paste("blue", (40, 0, 80, 40))
        exif = Image.Exif(); exif[274] = 3; exif[270] = "private camera description"
        output = BytesIO(); image.save(output, format="JPEG", exif=exif)
        data = self.create(avatarDataBase64=base64.b64encode(output.getvalue()).decode())
        with Image.open(BytesIO(self.client.get(data["avatarUrl"]).content)) as normalized:
            self.assertFalse(normalized.getexif())
            self.assertNotIn("exif", normalized.info)
            left = normalized.convert("RGB").getpixel((20, 128))
            right = normalized.convert("RGB").getpixel((235, 128))
            self.assertGreater(left[2], left[0])
            self.assertGreater(right[0], right[2])

    def test_definition_schemas_do_not_accept_private_avatar_upload_fields(self):
        self.membership.role = "owner"
        self.membership.save(update_fields=["role"])
        path = f"/api/workspaces/{self.workspace.pk}/agent-definitions"
        for value in (None, image_data()):
            response = self.send("post", path, {"name": "Managed", "avatarDataBase64": value})
            self.assertEqual(response.status_code, 400, response.content)
            self.assertEqual(response.json()["error"], "agent_definition_invalid")
        response = self.send("post", path, {"name": "Managed"})
        self.assertEqual(response.status_code, 201, response.content)
        definition = response.json()["definition"]
        response = self.send("patch", path + "/" + definition["id"], {"avatarDataBase64": image_data()})
        self.assertEqual(response.status_code, 400, response.content)
        self.assertEqual(response.json()["error"], "agent_definition_invalid")

    def test_managed_definition_configuration_cannot_be_overridden_with_custom_avatar(self):
        definition = AgentDefinition.objects.create(created_by=self.user, workspace=self.workspace, name="Managed")
        version = AgentDefinitionVersion.objects.create(definition=definition, version=1, name="Managed", description="", instructions="", avatar_kind="centaeris", published_by=self.user, published_at=timezone.now())
        definition.published_version = version; definition.save(update_fields=["published_version"])
        agent = Agent.objects.create(workspace=self.workspace, owner=self.user, definition=definition, name=version.name, description="", instructions="", avatar_kind=version.avatar_kind)
        result = self.send("patch", f"/api/agents/{agent.pk}", {"avatarDataBase64": image_data()})
        self.assertEqual(result.status_code, 409, result.content)
        self.assertEqual(result.json()["error"], "agent_configuration_managed")
        self.assertIsNone(self.client.get(f"/api/agents/{agent.pk}").json()["agent"]["avatarUrl"])

    def test_business_instance_cannot_accept_a_private_avatar_override(self):
        agent = Agent.objects.create(workspace=self.workspace, owner=self.user, name="Business instance", is_business_instance=True)
        response = self.send("patch", f"/api/agents/{agent.pk}", {"avatarDataBase64": image_data()})
        self.assertEqual(response.status_code, 409, response.content)
        self.assertEqual(response.json()["error"], "agent_business_configuration_managed")
        agent.refresh_from_db()
        self.assertIsNone(agent.avatar_image)
        self.assertEqual(agent.avatar_digest, "")

    def test_soft_delete_retains_avatar_for_restore_and_permanent_delete_erases_bytes(self):
        data = self.create(avatarDataBase64=image_data())
        path = f"/api/agents/{data['id']}"
        self.assertEqual(self.client.delete(path).status_code, 200)
        self.assertEqual(self.client.get(data["avatarUrl"]).status_code, 404)
        self.assertEqual(self.client.post(path + "/restore").status_code, 200)
        self.assertEqual(self.client.get(data["avatarUrl"]).status_code, 200)
        self.assertEqual(self.client.delete(path).status_code, 200)
        self.assertEqual(self.client.delete(path + "/trash").status_code, 200)
        agent = Agent.objects.get(pk=data["id"])
        self.assertIsNone(agent.avatar_image)
        self.assertEqual(agent.avatar_digest, "")

    def test_expired_agent_gc_erases_avatar_without_deleting_other_agents(self):
        from .deleted_resource_gc import expire_trash
        expired = self.create(avatarDataBase64=image_data())
        retained = self.create(avatarDataBase64=image_data(color="blue"))
        self.assertEqual(self.client.delete(f"/api/agents/{expired['id']}").status_code, 200)
        deleted_at = timezone.now() - timedelta(days=40)
        Agent.objects.filter(pk=expired["id"]).update(deletedAt=deleted_at)
        expire_trash(timezone.now() - timedelta(days=30), dry_run=False)
        agent = Agent.objects.get(pk=expired["id"])
        self.assertIsNotNone(agent.purgedAt)
        self.assertIsNone(agent.avatar_image)
        self.assertEqual(agent.avatar_digest, "")
        self.assertEqual(self.client.get(retained["avatarUrl"]).status_code, 200)

    def test_directory_does_not_load_private_image_bytes(self):
        from django.db import connection
        from django.test.utils import CaptureQueriesContext
        self.create(avatarDataBase64=image_data())
        with CaptureQueriesContext(connection) as captured:
            response = self.client.get(self.base)
        self.assertEqual(response.status_code, 200)
        self.assertIsNotNone(response.json()["agents"][0]["avatarUrl"])
        selects = [entry["sql"].lower() for entry in captured if 'from "app_core_agent"' in entry["sql"].lower()]
        self.assertTrue(selects)
        self.assertFalse(any('"avatar_image"' in query.split(' from ')[0] for query in selects))
