"""First-party credentials and actual MCP HTTP protocol boundary."""

import asyncio
from unittest.mock import patch

import httpx
from django.core import signing
from django.test import SimpleTestCase, TransactionTestCase, override_settings

from . import platform_mcp_auth as auth
from .material_access import MaterialAccessContext


def processing_specification():
    return {
        "schema": "knowledge.processing_specification.v1", "processorId": "centaeris.document.cpu",
        "processorVersion": "1.0.0", "executionImageDigest": "sha256:" + "1" * 64,
        "modelDigests": {"PP-OCRv6_small_det": "sha256:" + "2" * 64, "PP-OCRv6_small_rec": "sha256:" + "3" * 64},
        "options": {"renderDpi": 220, "maxInputBytes": 64 * 1024 * 1024, "maxRenderedPixelsPerPage": 16000000, "maxOutputBytes": 256 * 1024 * 1024},
    }


async def call_material_tool(token, name, arguments, *, call_id=None):
    from api.asgi import WorkspaceApplication
    application = WorkspaceApplication()
    async with application.mcp.router.lifespan_context(application.mcp):
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=application), base_url="http://localhost", headers={"Authorization": f"Bearer {token}", "Accept": "application/json, text/event-stream", "MCP-Protocol-Version": "2025-11-25"}) as client:
            headers = {"X-Workspace-Tool-Call-Id": call_id} if call_id is not None else {}
            return await client.post("/internal/mcp", headers=headers, json={"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": arguments}})


class PlatformMcpCredentialTests(SimpleTestCase):
    def setUp(self):
        self.context = MaterialAccessContext("run_1", "sha256:" + "a" * 64, {}, "sha256:" + "b" * 64)
        self.authorize = patch.object(auth, "authorize_context").start()
        self.addCleanup(patch.stopall)

    def test_ticket_is_short_lived_and_reauthorizes_each_use(self):
        with patch.object(auth.time, "time", return_value=1000):
            token = auth.issue_credential(self.context)["accessToken"]
            self.assertEqual(auth.verify_credential(token), self.context)
            self.assertEqual(auth.verify_credential(token), self.context)
        self.assertEqual(self.authorize.call_count, 3)
        with patch.object(auth.time, "time", return_value=1300):
            with self.assertRaises(auth.CredentialRejected):
                auth.verify_credential(token)

    def test_tampered_foreign_and_global_tokens_are_rejected(self):
        token = auth.issue_credential(self.context)["accessToken"]
        for value in (token + "x", "test-internal-token", signing.dumps({"agentRunId": "run_1"}), "x" * 17000):
            with self.subTest(value=value[:20]), self.assertRaises(auth.CredentialRejected):
                auth.verify_credential(value)

    def test_revocation_applies_to_already_issued_ticket(self):
        token = auth.issue_credential(self.context)["accessToken"]
        self.authorize.side_effect = auth.CredentialRejected()
        with self.assertRaises(auth.CredentialRejected):
            auth.verify_credential(token)

    def test_signed_unknown_claims_and_future_timestamp_are_rejected(self):
        with patch.object(auth.time, "time", return_value=1000):
            token = auth.issue_credential(self.context)["accessToken"]
            payload = signing.loads(token, key=auth.signing_key(), salt=auth.SALT)
            for change in ({"unknown": True}, {"issuedAt": 1001}, {"expiresAt": 999999}):
                invalid = signing.dumps({**payload, **change}, key=auth.signing_key(), salt=auth.SALT)
                with self.assertRaises(auth.CredentialRejected):
                    auth.verify_credential(invalid)


class PlatformMcpConnectionTests(TransactionTestCase):
    # Match the existing transactional suite's migration-data restoration mode.
    serialized_rollback = True

    def test_failed_authentication_releases_expired_database_connection(self):
        from asgiref.sync import sync_to_async
        from django.db import connection
        from . import platform_mcp as server

        def reject_after_query(token):
            with connection.cursor() as cursor:
                cursor.execute("SELECT 1")
            raise auth.CredentialRejected()

        async def scenario():
            app = server.create_mcp_app()
            with patch.object(server, "verify_credential", side_effect=reject_after_query):
                async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://localhost") as client:
                    result = await client.post("/internal/mcp", headers={"Authorization": "Bearer scoped"}, json={})
                    self.assertEqual(result.status_code, 401)
            try:
                self.assertTrue(await sync_to_async(lambda: connection.connection is None)())
            finally:
                await sync_to_async(lambda: connection.close())()

        # A production-style event loop, not AsyncToSync's caller-owned test transaction.
        asyncio.run(scenario())


class PlatformMcpTransportTests(SimpleTestCase):
    async def test_concurrent_requests_keep_separate_run_contexts(self):
        from . import platform_mcp as server
        app = server.create_mcp_app()
        def verify(token):
            return MaterialAccessContext(token, "digest", {}, "spec")
        def execute(context, name, arguments):
            return {"run": context.agent_run_id, "call": server.request_call_id.get()}
        with patch.object(server, "verify_credential", side_effect=verify), patch.object(server, "execute_tool", side_effect=execute):
            async with app.router.lifespan_context(app):
                async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://localhost") as client:
                    async def call(run):
                        return await client.post("/internal/mcp", headers={"Authorization": f"Bearer {run}", "X-Workspace-Tool-Call-Id": f"call-{run}", "Accept": "application/json, text/event-stream", "MCP-Protocol-Version": "2025-11-25"}, json={"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "list_materials", "arguments": {}}})
                    results = await asyncio.gather(call("run_a"), call("run_b"))
                    self.assertEqual([result.json()["result"]["structuredContent"]["run"] for result in results], ["run_a", "run_b"])
                    self.assertEqual([result.json()["result"]["structuredContent"]["call"] for result in results], ["call-run_a", "call-run_b"])
                    self.assertTrue(all(result.headers["cache-control"] == "no-store" for result in results))
        self.assertIsNone(server.request_context.get())
        self.assertIsNone(server.request_call_id.get())

    async def test_call_headers_reject_duplicates_empty_and_oversized_values(self):
        from . import platform_mcp as server
        app = server.create_mcp_app()
        with patch.object(server, "verify_credential") as verify:
            async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://localhost") as client:
                for values in (["a", "b"], [""], ["a" * 161]):
                    headers = [("Authorization", "Bearer scoped")] + [("X-Workspace-Tool-Call-Id", value) for value in values]
                    response = await client.post("/internal/mcp", headers=headers, json={})
                    self.assertEqual(response.status_code, 401)
                verify.assert_not_called()
        self.assertIsNone(server.request_call_id.get())

    @override_settings(PLATFORM_MCP_ALLOWED_HOSTS=["localhost", "127.0.0.1", "api"])
    async def test_compose_host_initializes_but_unknown_host_is_rejected(self):
        from . import platform_mcp as server
        app = server.create_mcp_app()
        with patch.object(server, "verify_credential", return_value=MaterialAccessContext("run", "digest", {}, "spec")):
            async with app.router.lifespan_context(app):
                async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://api:8000", headers={"Authorization": "Bearer scoped", "Accept": "application/json, text/event-stream"}) as client:
                    request = {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "docker-regression", "version": "1"}}}
                    response = await client.post("/internal/mcp", json=request)
                    self.assertEqual(response.status_code, 200, response.text)
                    self.assertEqual(response.json()["result"]["serverInfo"]["name"], "centaeris-workspace")
                    response = await client.post("/internal/mcp", headers={"Host": "attacker.example:8000"}, json=request)
                    self.assertEqual(response.status_code, 421)

    async def test_host_and_body_limits_precede_tool_execution(self):
        from . import platform_mcp as server
        app = server.create_mcp_app()
        with patch.object(server, "verify_credential", return_value=MaterialAccessContext("run", "digest", {}, "spec")), patch.object(server, "execute_tool") as execute:
            async with app.router.lifespan_context(app):
                async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://localhost", headers={"Authorization": "Bearer scoped", "Content-Type": "application/json", "Accept": "application/json, text/event-stream"}) as client:
                    response = await client.post("/internal/mcp", headers={"Host": "attacker.example"}, json={})
                    self.assertEqual(response.status_code, 421)
                    response = await client.post("/internal/mcp", content=b" " * (64 * 1024 + 1))
                    self.assertEqual(response.status_code, 413)
                    execute.assert_not_called()

    async def test_missing_global_duplicate_and_browser_credentials_fail_closed(self):
        from .platform_mcp import create_mcp_app
        app = create_mcp_app()
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://localhost") as client:
            for headers in ({}, {"X-Internal-Token": "test-internal-token"},
                            {"Authorization": "Bearer test-internal-token"},
                            [("Authorization", "Bearer a"), ("Authorization", "Bearer b")],
                            {"Authorization": "Bearer a", "Origin": "http://localhost"}):
                response = await client.post("/internal/mcp", headers=headers, json={})
                self.assertEqual(response.status_code, 401)
                self.assertEqual(response.headers["cache-control"], "no-store")

    async def test_initialize_discovery_strict_arguments_and_context_cleanup(self):
        from . import platform_mcp as server
        app = server.create_mcp_app()
        context = MaterialAccessContext("run_1", "digest", {}, "spec")
        headers = {"Authorization": "Bearer scoped", "Accept": "application/json, text/event-stream",
                   "MCP-Protocol-Version": "2025-11-25"}
        with patch.object(server, "verify_credential", return_value=context), patch.object(server, "execute_tool", return_value={"items": []}) as execute:
            async with app.router.lifespan_context(app):
                async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://localhost", headers=headers) as client:
                    response = await client.post("/internal/mcp", json={"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}})
                    self.assertEqual(response.status_code, 200, response.text)
                    self.assertEqual(response.json()["result"]["protocolVersion"], "2025-11-25")
                    response = await client.post("/internal/mcp", json={"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
                    listed = response.json()["result"]["tools"]
                    self.assertEqual({tool["name"] for tool in listed}, {"list_materials", "read_material", "search_materials", "read_material_result", "get_operation", "cancel_operation"})
                    for tool in listed:
                        self.assertFalse(tool["inputSchema"]["additionalProperties"])
                        self.assertNotIn("agent_run_id", tool["inputSchema"]["properties"])
                    for name, arguments in (("list_materials", {"agent_run_id": "other"}), ("read_material", {"input_ref": "x", "offset": True}), ("unknown", {})):
                        response = await client.post("/internal/mcp", json={"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": name, "arguments": arguments}})
                        self.assertTrue(response.json().get("error") or response.json().get("result", {}).get("isError"), response.text)
                    execute.assert_not_called()
                    response = await client.post("/internal/mcp", json={"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "list_materials", "arguments": {}}})
                    self.assertEqual(response.json()["result"]["structuredContent"], {"items": []})
                    execute.assert_called_once_with(context, "list_materials", {})
        self.assertIsNone(server.request_context.get())
