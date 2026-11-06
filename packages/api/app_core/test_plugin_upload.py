import hashlib
import io
import json
import stat
import tempfile
import warnings
import zipfile
from contextlib import nullcontext
from pathlib import Path
from unittest.mock import patch

from django.contrib.auth.models import User
from django.core.files.uploadedfile import SimpleUploadedFile
from django.test import SimpleTestCase, TestCase, override_settings

from app_core.models import Workspace, WorkspaceMembership
from app_core.plugin_install_source import (
    PluginInstallSourceError,
    UploadedZip,
)


def _zip_bytes(entries: list[tuple[str, bytes, int | None]]) -> bytes:
    buffer = io.BytesIO()
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", UserWarning)
        with zipfile.ZipFile(buffer, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for name, content, mode in entries:
                if mode is None:
                    archive.writestr(name, content)
                else:
                    info = zipfile.ZipInfo(name)
                    info.create_system = 3
                    info.external_attr = mode << 16
                    archive.writestr(info, content)
    return buffer.getvalue()


def _plugin_zip(name: str = "banana", version: str = "1.0.0", *, wrapper: bool = False) -> bytes:
    prefix = f"{name}/" if wrapper else ""
    manifest = {
        "name": name,
        "version": version,
        "paths": {},
        "interface": {
            "displayName": name.title(),
            "shortDescription": f"{name} test Plugin.",
            "capabilities": ["Synthetic capability"],
        },
    }
    return _zip_bytes(
        [(f"{prefix}.centaeris-plugin/plugin.json", json.dumps(manifest).encode(), None)]
    )


class UploadedZipTests(SimpleTestCase):
    def test_normalizes_root_and_single_wrapper_without_retaining_archive(self):
        for wrapper in (False, True):
            with self.subTest(wrapper=wrapper), tempfile.TemporaryDirectory() as temporary:
                staging = Path(temporary) / "stage"
                package = UploadedZip(
                    SimpleUploadedFile("banana.zip", _plugin_zip(wrapper=wrapper))
                ).normalize_into(staging)
                self.assertEqual(package, staging / "package")
                self.assertTrue((package / ".centaeris-plugin/plugin.json").is_file())
                self.assertFalse((staging / "upload.zip").exists())

    def test_rejects_unsafe_entry_types_and_paths(self):
        cases = {
            "traversal": [("../escape", b"bad", None)],
            "absolute": [("/escape", b"bad", None)],
            "symlink": [("link", b"target", stat.S_IFLNK | 0o777)],
            "device": [("device", b"bad", stat.S_IFCHR | 0o600)],
            "case collision": [("A/file", b"one", None), ("a/other", b"two", None)],
            "duplicate": [("same", b"one", None), ("same", b"two", None)],
            "file before child": [("parent", b"one", None), ("parent/child", b"two", None)],
            "file after child": [("parent/child", b"two", None), ("parent", b"one", None)],
        }
        for label, entries in cases.items():
            with self.subTest(label=label), tempfile.TemporaryDirectory() as temporary:
                with self.assertRaisesRegex(PluginInstallSourceError, "plugin_archive_invalid"):
                    UploadedZip(
                        SimpleUploadedFile("unsafe.zip", _zip_bytes(entries))
                    ).normalize_into(Path(temporary) / "stage")

    def test_rejects_entry_file_and_expanded_budgets_before_or_during_write(self):
        data = _zip_bytes([(".centaeris-plugin/plugin.json", b"0123456789", None)])
        for field in (
            "MAX_PLUGIN_ARCHIVE_FILE_BYTES",
            "MAX_PLUGIN_ARCHIVE_EXPANDED_BYTES",
        ):
            with (
                self.subTest(field=field),
                tempfile.TemporaryDirectory() as temporary,
                patch(f"app_core.plugin_install_source.{field}", 4),
                self.assertRaisesRegex(PluginInstallSourceError, "plugin_archive_too_large"),
            ):
                UploadedZip(SimpleUploadedFile("large.zip", data)).normalize_into(
                    Path(temporary) / "stage"
                )

    def test_rejects_archive_and_entry_count_budgets(self):
        data = _plugin_zip()
        with (
            tempfile.TemporaryDirectory() as temporary,
            patch("app_core.plugin_install_source.MAX_PLUGIN_ARCHIVE_BYTES", 4),
            self.assertRaisesRegex(PluginInstallSourceError, "plugin_archive_too_large"),
        ):
            UploadedZip(SimpleUploadedFile("large.zip", data)).normalize_into(
                Path(temporary) / "stage"
            )
        with (
            tempfile.TemporaryDirectory() as temporary,
            patch("app_core.plugin_install_source.MAX_PLUGIN_ARCHIVE_ENTRIES", 1),
            self.assertRaisesRegex(PluginInstallSourceError, "plugin_archive_too_large"),
        ):
            UploadedZip(
                SimpleUploadedFile(
                    "many.zip",
                    _zip_bytes([("one", b"1", None), ("two", b"2", None)]),
                )
            ).normalize_into(Path(temporary) / "stage")

    def test_requires_exactly_one_plugin_package_root(self):
        invalid = _zip_bytes(
            [
                ("banana/.centaeris-plugin/plugin.json", b"{}", None),
                ("kiwi/.centaeris-plugin/plugin.json", b"{}", None),
            ]
        )
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaisesRegex(
                PluginInstallSourceError, "plugin_package_layout_invalid"
            ):
                UploadedZip(SimpleUploadedFile("two.zip", invalid)).normalize_into(
                    Path(temporary) / "stage"
                )


class PluginUploadLifecycleTests(TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="centaeris-plugin-upload-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name) / "plugins"
        self.enterContext(override_settings(PLUGIN_CATALOG_ROOT=str(self.root)))
        for module in ("workspace_plugins", "http.workspaces"):
            self.enterContext(
                patch(f"app_core.{module}.plugin_lifecycle_lock", side_effect=nullcontext)
            )
        self.root.mkdir()
        self.inspect = self.enterContext(
            patch(
                "app_core.workspace_plugins.request_plugin_inspection",
                side_effect=self._inspect,
            )
        )
        self.admin = User.objects.create_user(
            username="plugin-admin@example.test",
            email="plugin-admin@example.test",
            password="test-password",
        )
        self.workspace = Workspace.objects.create(name="Plugins", createdBy=self.admin)
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.admin, role="owner")
        self.base = f"/api/workspaces/{self.workspace.id}/plugins"
        self.client.force_login(self.admin)

    def _inspect(self, package_path: str) -> dict:
        self.assertRegex(package_path, r"^\.upload-[0-9a-f]{32}/package$")
        package_root = self.root.joinpath(*package_path.split("/"))
        manifest_bytes = (
            package_root / ".centaeris-plugin/plugin.json"
        ).read_bytes()
        manifest = json.loads(manifest_bytes)
        return {
            "name": manifest["name"],
            "version": manifest["version"],
            "packageDigest": "sha256:" + hashlib.sha256(manifest_bytes).hexdigest(),
            "skills": [],
            "cli": [],
            "mcpServers": [],
            "hooks": [],
        }

    def upload(self, content: bytes, **extra):
        return self.client.post(
            f"{self.base}/upload",
            data={"file": SimpleUploadedFile("plugin.zip", content), **extra},
        )

    def test_upload_installs_lists_updates_and_removes_without_release_catalog(self):
        installed = self.upload(_plugin_zip(wrapper=True))
        self.assertEqual(installed.status_code, 200, installed.content)
        self.assertFalse(any(path.name.startswith(".upload-") for path in self.root.iterdir()))
        self.assertEqual(installed.json()["plugin"]["scope"], "workspace")
        self.assertEqual(self.client.get(self.base).json(), {"plugins": [installed.json()["plugin"]]})

        duplicate = self.upload(_plugin_zip())
        self.assertEqual(duplicate.status_code, 409, duplicate.content)
        self.assertEqual(duplicate.json(), {"error": "plugin_already_installed"})

        updated = self.upload(_plugin_zip(version="1.0.1"))
        self.assertEqual(updated.status_code, 200, updated.content)
        self.assertEqual(updated.json()["plugin"]["version"], "1.0.1")
        self.assertEqual(self.client.get(self.base).json()["plugins"][0]["version"], "1.0.1")

        removed = self.client.delete(f"{self.base}/banana/installation")
        self.assertEqual(removed.status_code, 204, removed.content)
        self.assertEqual(self.client.get(self.base).json(), {"plugins": []})

    def test_update_is_blocked_while_the_plugin_is_in_an_active_run(self):
        self.assertEqual(self.upload(_plugin_zip()).status_code, 200)
        with patch(
            "app_core.workspace_plugins._active_package",
            return_value=True,
        ):
            blocked = self.upload(_plugin_zip(version="1.0.1"))
        self.assertEqual(blocked.status_code, 409, blocked.content)
        self.assertEqual(blocked.json(), {"error": "plugin_in_active_agent_runs"})
        self.assertEqual(self.client.get(self.base).json()["plugins"][0]["version"], "1.0.0")

    def test_upload_strictly_rejects_extra_fields_and_invalid_carriers(self):
        extra = self.upload(_plugin_zip(), extra="unexpected")
        self.assertEqual(extra.status_code, 400, extra.content)
        self.assertEqual(extra.json(), {"error": "plugin_lifecycle_request_invalid"})
        invalid = self.upload(b"not a zip")
        self.assertEqual(invalid.status_code, 400, invalid.content)
        self.assertEqual(invalid.json(), {"error": "plugin_archive_invalid"})
        self.inspect.assert_not_called()

    def test_core_inspection_failure_never_mutates_installed_catalog(self):
        self.inspect.side_effect = ValueError("plugin_package_invalid")
        rejected = self.upload(_plugin_zip())
        self.assertEqual(rejected.status_code, 400, rejected.content)
        self.assertEqual(rejected.json(), {"error": "plugin_package_invalid"})
        self.assertEqual(self.client.get(self.base).json(), {"plugins": []})
        self.assertFalse(any(path.name.startswith(".upload-") for path in self.root.iterdir()))

    def test_workspace_member_cannot_upload_or_remove_plugins(self):
        member = User.objects.create_user(
            username="plugin-member@example.test",
            password="test-password",
        )
        WorkspaceMembership.objects.create(workspace=self.workspace, user=member, role="member")
        self.client.force_login(member)
        self.assertEqual(self.upload(_plugin_zip()).status_code, 404)
        self.assertEqual(self.client.delete(f"{self.base}/banana/installation").status_code, 404)
