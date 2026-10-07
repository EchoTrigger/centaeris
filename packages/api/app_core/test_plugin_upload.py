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

from app_core.credentials import encrypt_credential_secret
from app_core.models import (
    McpBearerCredential,
    Workspace,
    WorkspacePluginEnablement,
)
from app_core.plugin_catalog import load_plugin_catalog
from app_core.plugin_install_source import (
    PluginInstallSourceError,
    UploadedZip,
)
from app_core.plugin_lifecycle import initialize_plugin_catalog


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
        for module in (
            "plugin_catalog",
            "plugin_lifecycle",
            "http.plugin_lifecycle",
        ):
            self.enterContext(
                patch(f"app_core.{module}.plugin_lifecycle_lock", side_effect=nullcontext)
            )
        initialize_plugin_catalog()
        self.inspect = self.enterContext(
            patch(
                "app_core.plugin_lifecycle.request_plugin_inspection",
                side_effect=self._inspect,
            )
        )
        self.admin = User.objects.create_superuser(
            username="plugin-admin@example.test",
            email="plugin-admin@example.test",
            password="test-password",
        )
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
            "/api/admin/plugins/upload",
            data={"file": SimpleUploadedFile("plugin.zip", content), **extra},
        )

    def test_upload_installs_lists_updates_and_removes_without_release_catalog(self):
        installed = self.upload(_plugin_zip(wrapper=True))
        self.assertEqual(installed.status_code, 200, installed.content)
        self.assertEqual(
            installed.json(),
            {
                "plugin": {
                    "name": "banana",
                    "displayName": "Banana",
                    "shortDescription": "banana test Plugin.",
                    "capabilities": ["Synthetic capability"],
                    "version": "1.0.0",
                    "enabledWorkspaceCount": 0,
                    "credentialCount": 0,
                    "removable": True,
                    "errors": [],
                }
            },
        )
        self.assertFalse(any(path.name.startswith(".upload-") for path in self.root.iterdir()))
        self.assertEqual(
            self.client.get("/api/admin/plugins").json(),
            {"plugins": [installed.json()["plugin"]]},
        )

        duplicate = self.upload(_plugin_zip())
        self.assertEqual(duplicate.status_code, 409, duplicate.content)
        self.assertEqual(duplicate.json(), {"error": "plugin_already_installed"})

        updated = self.upload(_plugin_zip(version="1.0.1"))
        self.assertEqual(updated.status_code, 200, updated.content)
        self.assertEqual(updated.json()["plugin"]["version"], "1.0.1")
        self.assertEqual(load_plugin_catalog()["packages"][0]["version"], "1.0.1")

        removed = self.client.delete("/api/admin/plugins/banana")
        self.assertEqual(removed.status_code, 204, removed.content)
        self.assertEqual(self.client.get("/api/admin/plugins").json(), {"plugins": []})

    def test_update_is_blocked_while_the_plugin_is_in_an_active_run(self):
        self.assertEqual(self.upload(_plugin_zip()).status_code, 200)
        with patch(
            "app_core.http.plugin_lifecycle._plugin_has_active_agent_runs",
            return_value=True,
        ):
            blocked = self.upload(_plugin_zip(version="1.0.1"))
        self.assertEqual(blocked.status_code, 409, blocked.content)
        self.assertEqual(blocked.json(), {"error": "plugin_in_active_agent_runs"})
        self.assertEqual(load_plugin_catalog()["packages"][0]["version"], "1.0.0")

    def test_remove_reports_and_enforces_each_live_dependency(self):
        self.assertEqual(self.upload(_plugin_zip()).status_code, 200)
        workspace = Workspace.objects.create(name="Plugin use", createdBy=self.admin)
        WorkspacePluginEnablement.objects.create(
            workspace=workspace,
            pluginName="banana",
        )
        self.assertFalse(
            self.client.get("/api/admin/plugins").json()["plugins"][0]["removable"]
        )
        enabled = self.client.delete("/api/admin/plugins/banana")
        self.assertEqual(enabled.status_code, 409, enabled.content)
        self.assertEqual(enabled.json(), {"error": "plugin_enabled_in_workspaces"})
        WorkspacePluginEnablement.objects.all().delete()

        credential = McpBearerCredential.objects.create(
            plugin_name="banana",
            credential_ref="banana-token",
            display_name="Banana",
            encrypted_secret=encrypt_credential_secret("test-secret"),
            created_by=self.admin,
            updated_by=self.admin,
        )
        configured = self.client.delete("/api/admin/plugins/banana")
        self.assertEqual(configured.status_code, 409, configured.content)
        self.assertEqual(
            configured.json(), {"error": "plugin_credentials_configured"}
        )
        credential.delete()

        with (
            patch(
                "app_core.http.plugin_lifecycle._active_plugin_names",
                return_value={"banana"},
            ),
            patch(
                "app_core.http.plugin_lifecycle._plugin_has_active_agent_runs",
                return_value=True,
            ),
        ):
            self.assertFalse(
                self.client.get("/api/admin/plugins").json()["plugins"][0][
                    "removable"
                ]
            )
            active = self.client.delete("/api/admin/plugins/banana")
        self.assertEqual(active.status_code, 409, active.content)
        self.assertEqual(active.json(), {"error": "plugin_in_active_agent_runs"})

    def test_catalog_write_failure_rolls_back_package_bytes(self):
        self.assertEqual(self.upload(_plugin_zip()).status_code, 200)
        with patch(
            "app_core.plugin_lifecycle._write_catalog",
            side_effect=OSError("synthetic catalog failure"),
        ):
            failed = self.upload(_plugin_zip(version="1.0.1"))
        self.assertEqual(failed.status_code, 503, failed.content)
        self.assertEqual(failed.json(), {"error": "plugin_lifecycle_unavailable"})
        self.assertEqual(load_plugin_catalog()["packages"][0]["version"], "1.0.0")
        installed_manifest = json.loads(
            (self.root / "banana/.centaeris-plugin/plugin.json").read_text()
        )
        self.assertEqual(installed_manifest["version"], "1.0.0")
        self.assertFalse(
            any(
                path.name.startswith((".upload-", ".backup-"))
                for path in self.root.iterdir()
            )
        )

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
        self.assertEqual(load_plugin_catalog()["packages"], [])
        self.assertFalse(any(path.name.startswith(".upload-") for path in self.root.iterdir()))

    def test_non_superuser_cannot_upload_or_remove_plugins(self):
        member = User.objects.create_user(
            username="plugin-member@example.test",
            password="test-password",
        )
        self.client.force_login(member)
        self.assertEqual(self.upload(_plugin_zip()).status_code, 403)
        self.assertEqual(self.client.delete("/api/admin/plugins/banana").status_code, 403)
