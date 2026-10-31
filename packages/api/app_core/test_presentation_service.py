from contextlib import nullcontext
from importlib import import_module
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch
from django.test import SimpleTestCase


class PresentationApplicationTests(SimpleTestCase):
    async def test_http_requests_share_one_app_owner_and_internal_mcp_keeps_its_route(self):
        module = import_module("api.asgi")
        hub, django_app, mcp = SimpleNamespace(aclose=AsyncMock()), AsyncMock(), AsyncMock()
        with patch.object(module, "create_presentation_hub", return_value=hub) as create, patch.object(module, "create_mcp_app", return_value=mcp), patch.object(module, "django_application", django_app):
            owner = module.WorkspaceApplication()
            first, second = {"type": "http", "path": "/api/example"}, {"type": "http", "path": "/api/other"}
            receive, send = AsyncMock(), AsyncMock()
            await owner(first, receive, send)
            await owner(second, receive, send)
            self.assertNotIn("centaeris.presentationHub", first)
            self.assertNotIn("centaeris.presentationHub", second)
            for call in django_app.await_args_list:
                self.assertIs(call.args[0]["centaeris.presentationHub"], hub)
            create.assert_called_once()
            await owner({"type": "http", "path": "/internal/mcp"}, receive, send)
            mcp.assert_awaited_once()
            self.assertEqual(django_app.await_count, 2)

    async def test_shutdown_closes_observers_before_existing_mcp_lifecycle(self):
        module = import_module("api.asgi")
        order = []
        hub = SimpleNamespace(aclose=AsyncMock(side_effect=lambda: order.append("observer-close")))
        messages = iter([{"type": "lifespan.startup"}, {"type": "lifespan.shutdown"}])
        async def receive():
            return next(messages)
        async def mcp(scope, read, send):
            self.assertEqual((await read())["type"], "lifespan.startup")
            order.append("mcp-startup")
            await send({"type": "lifespan.startup.complete"})
            sample_task = owner.memory_observations._task
            self.assertIsNotNone(sample_task)
            self.assertFalse(sample_task.done())
            self.assertEqual((await read())["type"], "lifespan.shutdown")
            self.assertTrue(sample_task.done())
            self.assertIsNone(owner.memory_observations._task)
            order.append("mcp-shutdown")
            await send({"type": "lifespan.shutdown.complete"})
        with patch.object(module, "create_presentation_hub", return_value=hub), patch.object(module, "create_mcp_app", return_value=mcp):
            owner = module.WorkspaceApplication()
            send = AsyncMock()
            await owner({"type": "lifespan"}, receive, send)
        self.assertEqual(order, ["mcp-startup", "observer-close", "mcp-shutdown"])
        self.assertEqual([call.args[0]["type"] for call in send.await_args_list], ["lifespan.startup.complete", "lifespan.shutdown.complete"])


class PresentationServiceTests(SimpleTestCase):
    async def test_factory_is_lazy_and_database_reads_and_drain_have_statement_deadline(self):
        from . import presentation_service as service
        captured = {}
        def make_hub(reader, hints, keys, **options):
            captured.update(reader=reader, hints=hints, keys=keys, **options)
            return SimpleNamespace()
        scope = SimpleNamespace(workspace_id="workspace", agent_id="agent", session_id="session")
        connection = MagicMock()
        positions = dict(navigation=0, state=0, context=0, history=0, work=0)
        with patch.object(service, "PresentationObserverHub", side_effect=make_hub), patch.object(service, "_listener_connection_parameters") as parameters, patch.object(service, "connection", connection), patch.object(service.transaction, "atomic", return_value=nullcontext()), patch.object(service, "read_presentation_versions", return_value=positions) as read, patch.object(service, "drain_pending_changes", return_value=3) as drain:
            service.create_presentation_hub()
            parameters.assert_not_called()
            self.assertEqual(await captured["reader"](scope), positions)
            self.assertEqual(await captured["drain"](), 3)
            read.assert_called_once_with("workspace", "agent", "session")
            drain.assert_called_once_with(limit=256)
            cursor = connection.cursor.return_value.__enter__.return_value
            self.assertEqual(cursor.execute.call_count, 2)
            for call in cursor.execute.call_args_list:
                self.assertIn("statement_timeout", call.args[0])
                self.assertIn("5s", call.args[0])
            self.assertIn("agent:agent:history", captured["keys"](scope))

    def test_listener_connection_parameters_preserve_target_without_sync_cursor_or_pool(self):
        from . import presentation_service as service
        database = MagicMock(vendor="postgresql")
        database.get_connection_params.return_value = {
            "dbname": "fixture", "host": "fixture-db", "port": 5432,
            "cursor_factory": object(), "row_factory": object(), "pool": object(),
            "prepare_threshold": 5,
        }
        with patch.object(service, "connections", {"default": database}):
            parameters = service._listener_connection_parameters()
        self.assertEqual(parameters["dbname"], "fixture")
        self.assertEqual(parameters["host"], "fixture-db")
        self.assertEqual(parameters["port"], 5432)
        self.assertIsNone(parameters["prepare_threshold"])
        self.assertNotIn("cursor_factory", parameters)
        self.assertNotIn("row_factory", parameters)
        self.assertNotIn("pool", parameters)
