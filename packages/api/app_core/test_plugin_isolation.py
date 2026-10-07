"""Management isolation with synthetic packages; no Runtime, MCP, or shared DB."""

import hashlib
import json
import shutil
import tempfile
from contextlib import nullcontext
from pathlib import Path
from unittest.mock import patch

from django.contrib.auth.models import User
from django.test import TestCase, override_settings

from app_core.credentials import decrypt_credential_secret
from app_core.models import McpBearerCredential, Workspace, WorkspaceMembership, WorkspacePluginEnablement
from app_core.plugin_catalog import (
    MAX_PLUGIN_METADATA_BYTES, activation_digest, load_plugin_bearer_credential_refs,
    load_plugin_catalog, load_plugin_interfaces, plugin_activation_for_workspace,
)


class PluginIsolationTests(TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="centaeris-plugin-isolation-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.packages = []
        for name in ("banana", "kiwi"):
            manifest = self.root / name / ".centaeris-plugin" / "plugin.json"
            manifest.parent.mkdir(parents=True)
            manifest.write_text(json.dumps({
                "name": name, "version": "1.0.0", "paths": {},
                "interface": {"displayName": name.title()},
            }), encoding="utf-8")
            mcp_path = self.root / name / "mcp.json"
            mcp_content = json.dumps(
                {"schema": "mcp_servers_v1", "servers": []}
            ).encode("utf-8")
            mcp_path.write_bytes(mcp_content)
            mcp_digest = hashlib.sha256(b"centaeris.plugin.tree.v1\0")
            for data in (b"mcp.json", mcp_content):
                mcp_digest.update(len(data).to_bytes(8, "big"))
                mcp_digest.update(data)
            self.packages.append({
                "name": name, "version": "1.0.0", "packageDigest": "sha256:" + "a" * 64,
                "skills": [], "cli": [],
                "hooks": [{"path": "hooks.json", "digest": "sha256:" + "b" * 64}],
                "mcpServers": [{"path": "mcp.json", "digest": f"sha256:{mcp_digest.hexdigest()}"}],
            })
        self.write_catalog()
        self.enterContext(override_settings(PLUGIN_CATALOG_ROOT=self.root))
        # SQLite gate replaces only the PostgreSQL lock, not authorization or mutations.
        for module in ("plugin_catalog", "http.workspaces", "http.mcp_credentials"):
            self.enterContext(patch(f"app_core.{module}.plugin_lifecycle_lock", side_effect=nullcontext))
        self.mcp = self.enterContext(patch("app_core.http.workspaces.request_workspace_mcp_catalog"))
        self.hooks = self.enterContext(patch("app_core.http.workspaces.request_workspace_hook_catalog"))
        self.logs = self.enterContext(patch("app_core.http.workspaces.logger"))
        self.mcp.side_effect = lambda activation: self.projection(activation, "servers")
        self.hooks.side_effect = lambda activation: self.projection(activation, "hooks")
        self.user = User.objects.create_superuser(username="owner", email="owner@example.invalid", password="test-password")
        self.workspace = Workspace.objects.create(name="Isolation", createdBy=self.user)
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        self.client.force_login(self.user)
        self.url = f"/api/workspaces/{self.workspace.id}/plugins"

    def write_catalog(self):
        (self.root / "catalog.snapshot.json").write_text(json.dumps({
            "schema": "plugin_activation_snapshot_v1",
            "digest": activation_digest(self.packages), "packages": self.packages,
        }), encoding="utf-8")

    def projection(self, activation, field):
        self.assertEqual(len(activation["packages"]), 1)
        self.assertEqual(activation["digest"], activation_digest(activation["packages"]))
        name = activation["packages"][0]["name"]
        if name == "kiwi":
            raise RuntimeError("synthetic invalid declaration")
        return {"plugins": [{"pluginName": name, field: []}]}

    def write_mcp(self, payload):
        path = "mcp.json"
        content = json.dumps(payload).encode("utf-8")
        (self.root / "kiwi" / path).write_bytes(content)
        digest = hashlib.sha256(b"centaeris.plugin.tree.v1\0")
        for data in (path.encode("utf-8"), content):
            digest.update(len(data).to_bytes(8, "big"))
            digest.update(data)
        self.packages[1]["mcpServers"] = [{"path": path, "digest": f"sha256:{digest.hexdigest()}"}]
        self.write_catalog()

    def test_credential_refs_do_not_depend_on_complete_tools_or_runtime(self):
        self.write_mcp({"schema": "mcp_servers_v1", "servers": [
            {"id": f"source-{index}", "transport": {
                "type": "streamableHttp", "url": "https://kiwi.invalid/mcp",
                "bearerCredentialRef": "kiwi-token",
            }, "tools": [{"sourceName": "missing-schema"}]}
            for index in range(9)
        ]})
        listed = self.client.get(self.url).json()["plugins"][1]
        self.assertEqual(listed["mcpCredentialRefs"], ["kiwi-token"])
        self.mcp.assert_not_called()
        self.hooks.assert_not_called()
        detail = self.client.get(f"{self.url}/kiwi").json()["plugin"]
        self.assertEqual(detail["mcpCredentialRefs"], ["kiwi-token"])
        self.assertIsNone(detail["mcpServers"])
        self.assertIn("workspace_mcp_catalog_unavailable", detail["errors"])
        self.assertEqual(self.toggle("kiwi", True).status_code, 409)

    def test_invalid_credential_metadata_is_local_and_never_guessed(self):
        for transport in (
            {"type": "streamableHttp", "url": "https://kiwi.invalid", "credentialRef": "kiwi-token"},
            {"type": "streamableHttp", "url": "https://kiwi.invalid", "bearerCredentialRef": None},
            {"type": "streamableHttp", "url": "https://kiwi.invalid", "bearerCredentialRef": "INVALID"},
            {"type": "unknown"},
        ):
            with self.subTest(transport=transport):
                self.write_mcp({"schema": "mcp_servers_v1", "servers": [{"transport": transport}]})
                listed = self.client.get(self.url)
                self.assertEqual(listed.status_code, 200)
                self.assertEqual(listed.json()["plugins"][0]["mcpCredentialRefs"], [])
                bad = listed.json()["plugins"][1]
                self.assertIsNone(bad["mcpCredentialRefs"])
                self.assertEqual(bad["errors"], ["plugin_credentials_unavailable"])
                self.assertEqual(self.toggle("kiwi", False).status_code, 200)
        self.write_mcp({"schema": "mcp_servers_v1", "servers": []})
        (self.root / "kiwi" / "mcp.json").write_text("{}", encoding="utf-8")
        bad = self.client.get(self.url).json()["plugins"][1]
        self.assertIsNone(bad["mcpCredentialRefs"])
        self.assertEqual(bad["errors"], ["plugin_credentials_unavailable"])
        (self.root / "kiwi" / "mcp.json").unlink()
        self.assertEqual(self.client.get(self.url).status_code, 200)
        self.mcp.assert_not_called()
        self.hooks.assert_not_called()

    def test_oversized_metadata_is_rejected_before_parsing_and_isolated(self):
        for relative, expected_error, loader in (
            ("kiwi/.centaeris-plugin/plugin.json", "plugin_manifest_invalid",
             lambda: load_plugin_interfaces({"packages": [self.packages[1]]})),
            ("kiwi/mcp.json", "plugin_credentials_unavailable",
             lambda: load_plugin_bearer_credential_refs(self.packages[1])),
        ):
            with self.subTest(resource=relative):
                self.write_mcp({"schema": "mcp_servers_v1", "servers": []})
                path = self.root / relative
                original = path.read_bytes()
                try:
                    # Invalid UTF-8 also proves size rejection happens before decoding.
                    path.write_bytes(b"\xff" * (MAX_PLUGIN_METADATA_BYTES + 1))
                    with patch("app_core.plugin_catalog.json.loads") as parse:
                        with self.assertRaisesRegex(ValueError, "exceeds byte budget"):
                            loader()
                        parse.assert_not_called()
                    listed = self.client.get(self.url)
                    self.assertEqual(listed.status_code, 200)
                    self.assertEqual(listed.json()["plugins"][0]["errors"], [])
                    self.assertEqual(listed.json()["plugins"][1]["errors"], [expected_error])
                    WorkspacePluginEnablement.objects.create(workspace=self.workspace, pluginName="kiwi")
                    self.assertEqual(self.toggle("kiwi", False).status_code, 200)
                    self.assertFalse(WorkspacePluginEnablement.objects.exists())
                finally:
                    path.write_bytes(original)
        self.mcp.assert_not_called()
        self.hooks.assert_not_called()

    def test_oversized_catalog_fails_closed_before_parsing(self):
        (self.root / "catalog.snapshot.json").write_bytes(b"\xff" * (MAX_PLUGIN_METADATA_BYTES + 1))
        with patch("app_core.plugin_catalog.json.loads") as parse:
            with self.assertRaisesRegex(ValueError, "exceeds byte budget"):
                load_plugin_catalog()
            parse.assert_not_called()
        response = self.client.get(self.url)
        self.assertEqual(response.status_code, 503)
        self.assertEqual(response.json()["error"], "plugin_catalog_invalid")
        self.mcp.assert_not_called()
        self.hooks.assert_not_called()

    def test_mcp_byte_budget_is_shared_across_files_and_accepts_exact_limit(self):
        payload = json.dumps({"schema": "mcp_servers_v1", "servers": []}).encode()
        content = payload.ljust(MAX_PLUGIN_METADATA_BYTES // 2, b" ")
        resources = []
        for name in ("a.json", "b.json", "c.json"):
            (self.root / "kiwi" / name).write_bytes(content)
            digest = hashlib.sha256(b"centaeris.plugin.tree.v1\0")
            for data in (name.encode(), content):
                digest.update(len(data).to_bytes(8, "big"))
                digest.update(data)
            resources.append({"path": name, "digest": f"sha256:{digest.hexdigest()}"})
        package = self.packages[1]
        package["mcpServers"] = resources[:2]
        self.assertEqual(load_plugin_bearer_credential_refs(package), [])
        package["mcpServers"] = resources
        with patch("app_core.plugin_catalog.json.loads", wraps=json.loads) as parse:
            with self.assertRaisesRegex(ValueError, "exceeds byte budget"):
                load_plugin_bearer_credential_refs(package)
            self.assertEqual(parse.call_count, 2)
        self.write_catalog()
        bad = self.client.get(self.url).json()["plugins"][1]
        self.assertEqual(bad["errors"], ["plugin_credentials_unavailable"])

    def toggle(self, name, enabled):
        return self.client.patch(f"{self.url}/{name}", data=json.dumps({"enabled": enabled}), content_type="application/json")

    def test_inventory_and_healthy_plugin_survive_bad_mcp_and_hooks(self):
        response = self.client.get(self.url)
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual([item["name"] for item in response.json()["plugins"]], ["banana", "kiwi"])
        self.assertTrue(all(item["mcpServers"] is None and item["hooks"] is None for item in response.json()["plugins"]))
        self.mcp.assert_not_called()
        self.hooks.assert_not_called()
        bad = self.client.get(f"{self.url}/kiwi").json()["plugin"]
        self.assertEqual(bad["errors"], ["workspace_mcp_catalog_unavailable", "workspace_hook_catalog_unavailable"])
        self.assertIsNone(bad["mcpServers"])
        good = self.client.get(f"{self.url}/banana").json()["plugin"]
        self.assertEqual((good["errors"], good["mcpServers"], good["hooks"]), ([], [], []))
        self.assertEqual(self.toggle("kiwi", True).status_code, 409)
        self.assertFalse(WorkspacePluginEnablement.objects.exists())
        self.assertEqual(self.toggle("banana", True).status_code, 200)
        self.assertEqual(list(self.workspace.pluginEnablements.values_list("pluginName", flat=True)), ["banana"])

    def test_empty_mcp_and_hook_contributions_skip_runtime_inspection(self):
        self.packages[0]["mcpServers"] = []
        self.packages[0]["hooks"] = []
        self.write_catalog()

        plugin = self.client.get(f"{self.url}/banana").json()["plugin"]

        self.assertEqual(plugin["errors"], [])
        self.assertEqual(plugin["mcpServers"], [])
        self.assertEqual(plugin["hooks"], [])
        self.mcp.assert_not_called()
        self.hooks.assert_not_called()

    def test_disabled_and_enabled_bad_plugins_remain_visible_and_disable_never_calls_runtime(self):
        WorkspacePluginEnablement.objects.create(workspace=self.workspace, pluginName="kiwi")
        self.mcp.side_effect = self.hooks.side_effect = RuntimeError("Runtime offline")
        self.assertEqual(self.client.get(self.url).status_code, 200)
        response = self.toggle("kiwi", False)
        self.assertEqual(response.status_code, 200, response.content)
        self.assertFalse(response.json()["plugin"]["enabled"])
        self.mcp.assert_not_called()
        self.hooks.assert_not_called()
        self.assertFalse(WorkspacePluginEnablement.objects.exists())
        self.assertEqual(self.toggle("banana", True).status_code, 409)
        self.assertFalse(WorkspacePluginEnablement.objects.exists())

    def test_hook_failure_is_explicit_without_discarding_valid_mcp(self):
        self.hooks.side_effect = RuntimeError("invalid hook")
        plugin = self.client.get(f"{self.url}/banana").json()["plugin"]
        self.assertEqual(plugin["mcpServers"], [])
        self.assertIsNone(plugin["hooks"])
        self.assertEqual(plugin["errors"], ["workspace_hook_catalog_unavailable"])
        self.assertEqual(self.toggle("banana", True).status_code, 409)

    def test_runtime_timeout_preserves_inventory_and_blocks_only_enablement(self):
        self.mcp.side_effect = self.hooks.side_effect = TimeoutError("synthetic timeout")
        self.assertEqual(self.client.get(self.url).status_code, 200)
        detail = self.client.get(f"{self.url}/banana")
        self.assertEqual(detail.status_code, 200, detail.content)
        self.assertEqual(len(detail.json()["plugin"]["errors"]), 2)
        self.assertEqual(self.toggle("banana", True).status_code, 409)
        self.assertEqual(self.toggle("banana", False).status_code, 200)

    def test_package_update_during_enablement_requires_a_new_inspection(self):
        def replace_package(activation):
            self.packages[0]["packageDigest"] = "sha256:" + "b" * 64
            self.write_catalog()
            return self.projection(activation, "hooks")
        self.hooks.side_effect = replace_package
        response = self.toggle("banana", True)
        self.assertEqual(response.status_code, 409, response.content)
        self.assertEqual(response.json()["error"], "workspace_plugin_package_changed")
        self.assertFalse(WorkspacePluginEnablement.objects.exists())

    def test_malformed_runtime_result_cannot_enable_or_break_another_plugin(self):
        for result in (
            {"plugins": [{"pluginName": "wrong", "servers": []}]},
            {"plugins": [{"pluginName": "banana", "servers": [{"id": "missing-fields"}]}]},
            {"plugins": [{"pluginName": "banana", "servers": [], "oldAlias": True}]},
        ):
            with self.subTest(result=result):
                self.mcp.side_effect = None
                self.mcp.return_value = result
                response = self.client.get(f"{self.url}/banana")
                self.assertEqual(response.status_code, 200, response.content)
                self.assertEqual(response.json()["plugin"]["errors"], ["workspace_mcp_catalog_unavailable"])
                self.assertEqual(self.toggle("banana", True).status_code, 409)

    def test_broken_or_missing_manifest_is_local_and_can_be_disabled(self):
        path = self.root / "kiwi" / ".centaeris-plugin" / "plugin.json"
        for content in ("{", None):
            with self.subTest(content=content):
                if content is None:
                    path.unlink()
                else:
                    path.write_text(content, encoding="utf-8")
                listed = self.client.get(self.url)
                self.assertEqual(listed.status_code, 200, listed.content)
                self.assertEqual(listed.json()["plugins"][1]["errors"], ["plugin_manifest_invalid"])
                self.assertEqual(self.toggle("kiwi", True).status_code, 409)
                self.assertEqual(self.toggle("kiwi", False).status_code, 200)
                self.assertEqual(self.toggle("banana", True).status_code, 200)
        shutil.rmtree(path.parent.parent)
        self.assertEqual(self.client.get(self.url).status_code, 200)
        self.assertEqual(self.toggle("kiwi", False).status_code, 200)
        with self.assertRaisesRegex(ValueError, "plugin_package_missing:kiwi"):
            load_plugin_catalog()

    def test_global_catalog_digest_corruption_still_fails_closed(self):
        payload = json.loads((self.root / "catalog.snapshot.json").read_text())
        payload["digest"] = "sha256:" + "0" * 64
        (self.root / "catalog.snapshot.json").write_text(json.dumps(payload))
        for response in (self.client.get(self.url), self.client.get(f"{self.url}/banana"), self.toggle("banana", False)):
            self.assertEqual(response.status_code, 503)
            self.assertEqual(response.json()["error"], "plugin_catalog_invalid")
        self.mcp.assert_not_called()
        self.hooks.assert_not_called()

    def test_credentials_can_be_created_rotated_and_deleted_for_a_bad_plugin(self):
        self.mcp.side_effect = self.hooks.side_effect = RuntimeError("Runtime offline")
        url = "/api/admin/mcp-bearer-credentials"
        response = self.client.post(url, data=json.dumps({
            "pluginName": "kiwi", "credentialRef": "kiwi-token", "displayName": "Kiwi",
            "secret": "synthetic-first-secret",
        }), content_type="application/json")
        self.assertEqual(response.status_code, 201, response.content)
        identity = response.json()["credential"]["id"]
        self.assertNotIn("synthetic-first-secret", response.content.decode())
        self.assertEqual(self.client.get(url).status_code, 200)
        rotated = self.client.post(f"{url}/{identity}/rotate", data=json.dumps({"secret": "synthetic-next-secret"}), content_type="application/json")
        self.assertEqual(rotated.status_code, 200, rotated.content)
        self.assertEqual(rotated.json()["credential"]["version"], 2)
        self.assertEqual(self.client.delete(f"{url}/{identity}").status_code, 204)
        self.assertFalse(McpBearerCredential.objects.exists())
        self.mcp.assert_not_called()
        self.hooks.assert_not_called()

    def test_valid_mcp_metadata_retains_exact_contract_without_exposing_secrets(self):
        self.mcp.side_effect = None
        self.mcp.return_value = {"plugins": [{"pluginName": "banana", "servers": [{
            "id": "banana-source", "modelContractDigest": "sha256:" + "c" * 64,
            "transport": {"type": "streamableHttp", "endpoint": "https://banana.invalid/mcp"},
            "auth": {"type": "bearer", "credentialRef": "banana-token"},
            "startupTimeoutMs": 1000, "toolTimeoutMs": 1000,
            "tools": [{"sourceName": "search", "name": "banana_search", "description": "Search bananas.",
                       "inputSchema": {"type": "object"},
                       "concurrencySafe": True, "scopes": []}],
        }]}]}
        self.client.post("/api/admin/mcp-bearer-credentials", data=json.dumps({
            "pluginName": "banana", "credentialRef": "banana-token", "displayName": "Banana",
            "secret": "synthetic-private-token",
        }), content_type="application/json")
        response = self.client.get(f"{self.url}/banana")
        self.assertEqual(response.status_code, 200, response.content)
        plugin = response.json()["plugin"]
        self.assertEqual(plugin["errors"], [])
        self.assertEqual(plugin["mcpServers"][0]["tools"], self.mcp.return_value["plugins"][0]["servers"][0]["tools"])
        self.assertTrue(plugin["mcpServers"][0]["auth"]["credentialConfigured"])
        self.assertNotIn("synthetic-private-token", response.content.decode())

    def test_bare_and_prefixed_token_inputs_store_only_the_token_on_create_and_rotate(self):
        url = "/api/admin/mcp-bearer-credentials"
        for supplied in ("  synthetic-token  ", "  Bearer synthetic-token  ", "bearer synthetic-token"):
            with self.subTest(format="prefixed" if "bearer" in supplied.lower() else "bare"):
                response = self.client.post(url, data=json.dumps({
                    "pluginName": "kiwi", "credentialRef": "kiwi-token", "displayName": "Kiwi",
                    "secret": supplied,
                }), content_type="application/json")
                self.assertEqual(response.status_code, 201, response.content)
                stored = McpBearerCredential.objects.get()
                self.assertEqual(decrypt_credential_secret(stored.encrypted_secret), "synthetic-token")
                self.assertNotIn("synthetic-token", response.content.decode())
                rotated = self.client.post(f"{url}/{stored.id}/rotate", data=json.dumps({"secret": supplied}), content_type="application/json")
                self.assertEqual(rotated.status_code, 200, rotated.content)
                stored.refresh_from_db()
                self.assertEqual(decrypt_credential_secret(stored.encrypted_secret), "synthetic-token")
                self.assertNotIn("synthetic-token", rotated.content.decode())
                stored.delete()

    def test_invalid_token_inputs_cannot_create_or_overwrite_a_credential(self):
        url = "/api/admin/mcp-bearer-credentials"
        body = {"pluginName": "kiwi", "credentialRef": "kiwi-token", "displayName": "Kiwi", "secret": "synthetic-original"}
        response = self.client.post(url, data=json.dumps(body), content_type="application/json")
        stored = McpBearerCredential.objects.get(id=response.json()["credential"]["id"])
        encrypted = stored.encrypted_secret
        for invalid in ("", "  ", "Bearer ", "Bearer Bearer token", "Bearer token\r\nX-Injected: bad", "Authorization: Bearer token", "Bearer 中文", "a" * 4097):
            with self.subTest(length=len(invalid)):
                created = self.client.post(url, data=json.dumps({**body, "credentialRef": "other-token", "secret": invalid}), content_type="application/json")
                rotated = self.client.post(f"{url}/{stored.id}/rotate", data=json.dumps({"secret": invalid}), content_type="application/json")
                self.assertEqual(created.status_code, 400, created.content)
                self.assertEqual(rotated.status_code, 400, rotated.content)
                stored.refresh_from_db()
                self.assertEqual(stored.encrypted_secret, encrypted)
                self.assertEqual(stored.version, 1)
                self.assertEqual(McpBearerCredential.objects.count(), 1)

    def test_activation_never_silently_drops_an_enabled_broken_plugin(self):
        WorkspacePluginEnablement.objects.create(workspace=self.workspace, pluginName="kiwi")
        self.assertEqual([item["name"] for item in plugin_activation_for_workspace(self.workspace)["packages"]], ["kiwi"])

    def test_detail_and_mutations_preserve_membership_and_admin_boundaries(self):
        other = User.objects.create_user(username="other")
        self.client.force_login(other)
        self.assertEqual(self.client.get(f"{self.url}/banana").status_code, 404)
        self.assertEqual(self.toggle("banana", False).status_code, 404)
        WorkspaceMembership.objects.create(workspace=self.workspace, user=other, role="member")
        self.assertEqual(self.client.get(f"{self.url}/banana").status_code, 200)
        self.assertEqual(self.toggle("banana", True).status_code, 404)
        self.assertEqual(self.client.get("/api/admin/mcp-bearer-credentials").status_code, 403)

    def test_empty_inventory_and_unknown_plugin(self):
        self.packages = []
        self.write_catalog()
        self.assertEqual(self.client.get(self.url).json(), {"plugins": []})
        self.assertEqual(self.client.get(f"{self.url}/missing").status_code, 404)
        self.assertEqual(self.toggle("missing", False).status_code, 404)
        self.mcp.assert_not_called()
        self.hooks.assert_not_called()
