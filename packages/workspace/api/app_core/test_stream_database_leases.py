"""A paused SSE request must leave the real Django pool available to other requests."""
import asyncio
from copy import deepcopy
from contextvars import ContextVar
import http.client
from importlib import import_module
import json
import os
import socket
import threading
import time
from unittest.mock import patch
import weakref

from asgiref.sync import ThreadSensitiveContext, sync_to_async
from django.db import connections
from django.db.backends.postgresql.base import DatabaseWrapper
from django.conf import settings
from django.contrib.auth import get_user_model
from django.core import signals
from django.test import TransactionTestCase, override_settings
from django.urls import path
import psycopg
import uvicorn

from api.urls import urlpatterns as application_patterns
from . import agent_run_stream, tests as fixtures
from .http.security import ProductSessionAuth
from .http import streaming
from .models import AgentRun, ModelConfig, Workspace
from .test_model_database_leases import database_probe
from .test_workspace_file_http import WorkspaceFileHttpFixture, document, file_descriptor
from .workspace_file_objects import FileSequence

from .agent_run_stream import _load_terminal_sequence
from .http.streaming import _prepare_agent_run_stream
from .http.stream_response import stream_database_call


urlpatterns = [path("lease-test/database-probe", database_probe), *application_patterns]
_lease_request = ContextVar("stream_lease_test_request", default="")
_lease_stage = ContextVar("stream_lease_test_stage", default="http")


class StreamDatabaseLeaseTests(TransactionTestCase):
    # Match the suite snapshot so post_migrate cannot duplicate restored content types.
    serialized_rollback = True

    def setUp(self):
        connections.close_all()
        previous = DatabaseWrapper._connection_pools.pop("default", None)
        if previous is not None:
            previous.close()
        self.config = connections.settings["default"]
        self.options = deepcopy(self.config.get("OPTIONS", {}))
        self.config["OPTIONS"] = {
            **self.options,
            "pool": {"min_size": 0, "max_size": 1, "timeout": .3},
        }

    def tearDown(self):
        connections.close_all()
        pool = DatabaseWrapper._connection_pools.pop("default", None)
        if pool is not None:
            pool.close()
        self.config["OPTIONS"] = self.options

    def _other_request_can_query_while_stream_waits(self, stream_query):
        async def exercise():
            ready = asyncio.Event()
            release = asyncio.Event()

            async def stream_request():
                async with ThreadSensitiveContext():
                    try:
                        await stream_query()
                        ready.set()
                        await release.wait()
                    finally:
                        await sync_to_async(connections.close_all, thread_sensitive=True)()

            def lifecycle_query():
                try:
                    with connections["default"].cursor() as cursor:
                        cursor.execute("SELECT 1")
                        return cursor.fetchone()[0]
                finally:
                    connections.close_all()

            task = asyncio.create_task(stream_request())
            try:
                await asyncio.wait_for(ready.wait(), 3)
                self.assertFalse(task.done(), "the SSE request must still be open")
                async with ThreadSensitiveContext():
                    value = await asyncio.wait_for(
                        sync_to_async(lifecycle_query, thread_sensitive=True)(), 2
                    )
                self.assertEqual(value, 1)
                self.assertFalse(task.done(), "query capacity must not require closing SSE")
            finally:
                release.set()
                await asyncio.wait_for(task, 3)

        asyncio.run(exercise())

    def test_postgres_stream_poll_releases_pool_lease_before_redis_wait(self):
        self._other_request_can_query_while_stream_waits(
            lambda: _load_terminal_sequence("missing-stream-run")
        )

    def test_stream_preparation_releases_pool_lease_before_response_wait(self):
        self._other_request_can_query_while_stream_waits(
            lambda: _prepare_agent_run_stream(
                user_id=0, session_id="missing-session", agent_run_id="missing-run",
                last_event_id="",
            )
        )

    def test_failed_stream_database_call_returns_pool_lease_and_preserves_error(self):
        @stream_database_call
        def failed_query():
            with connections["default"].cursor() as cursor:
                cursor.execute("SELECT 1")
                cursor.fetchone()
            raise ValueError("stream-query-failed")

        async def query():
            with self.assertRaisesRegex(ValueError, "stream-query-failed"):
                await failed_query()

        self._other_request_can_query_while_stream_waits(query)


class HttpDatabaseLeaseFixture:
    """Exercise real transport closure through public auth and the product handler."""

    serialized_rollback = True

    def setUp(self):
        super().setUp()
        self._prepare_http_fixtures()
        self.observer = psycopg.connect(**(connections["default"].get_connection_params() | {
            "application_name": "centaeris-stream-lease-observer", "connect_timeout": 2,
        }), autocommit=True)
        self.addCleanup(self.observer.close)

        connections.close_all()
        previous = DatabaseWrapper._connection_pools.pop("default", None)
        if previous is not None:
            previous.close()
        config = connections.settings["default"]
        options = deepcopy(config.get("OPTIONS", {}))
        config["OPTIONS"] = {
            **options, "pool": {"min_size": 0, "max_size": 1, "timeout": .3},
        }

        def restore_pool():
            connections.close_all()
            pool = DatabaseWrapper._connection_pools.pop("default", None)
            if pool is not None:
                pool.close()
            config["OPTIONS"] = options

        self.addCleanup(restore_pool)
        self.pool = connections["default"].pool
        self.events = []
        self.trace_lock = threading.Lock()
        self.borrowed = {}
        self.next_lease = 0
        self.physical = {}
        self.requests = {}
        self._observe_pool()
        self._observe_database_stages()
        self._start_http_server()

    def _prepare_http_fixtures(self):
        user = get_user_model().objects.create_user(username="http-stream-lease", password="test")
        workspace = Workspace.objects.create(name="HTTP stream leases", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(displayName="HTTP stream leases")
        self.runs = {}
        for mode in ("normal", "terminal-disconnect", "orm-disconnect", "session-save-disconnect"):
            self.runs[mode] = []
            for _ in range(5):
                session = fixtures.create_session(workspace=workspace, owner=user)
                run = AgentRun.objects.create(
                    workspace=workspace, session=session, user=user,
                    modelConfig=model, prompt="HTTP stream lease", status="running",
                )
                fixtures.create_agent_run_authorization(run)
                fixtures.append_started(run)
                if mode != "orm-disconnect":
                    fixtures.append_completed(run)
                self.runs[mode].append((session.id, run.id))
        self.client.force_login(user)
        self.cookie = f"{settings.SESSION_COOKIE_NAME}={self.client.cookies[settings.SESSION_COOKIE_NAME].value}"

    def _record(self, event, **details):
        with self.trace_lock:
            self.events.append({
                "event": event, "request": _lease_request.get(), "stage": _lease_stage.get(),
                "thread": threading.get_ident(), "at": time.monotonic(), **details,
            })

    def _observe_pool(self):
        original_connect = self.pool._connect
        original_get = self.pool.getconn
        original_put = self.pool.putconn

        def connect(*args, **kwargs):
            value = original_connect(*args, **kwargs)
            with self.trace_lock:
                self.physical[id(value)] = (weakref.ref(value), value.info.backend_pid)
            return value

        def get(*args, **kwargs):
            value = original_get(*args, **kwargs)
            with self.trace_lock:
                self.next_lease += 1
                generation = self.next_lease
                self.borrowed[id(value)] = {"request": _lease_request.get(), "generation": generation}
            self._record("getconn", connection=id(value), pool=id(self.pool), generation=generation)
            return value

        def put(value, *args, **kwargs):
            with self.trace_lock:
                lease = self.borrowed.get(id(value))
            result = original_put(value, *args, **kwargs)
            with self.trace_lock:
                if self.borrowed.get(id(value)) == lease:
                    self.borrowed.pop(id(value), None)
            self._record("putconn", connection=id(value), pool=id(self.pool),
                generation=lease["generation"] if lease is not None else None)
            return result

        self.enterContext(patch.object(self.pool, "_connect", connect))
        self.enterContext(patch.object(self.pool, "getconn", get))
        self.enterContext(patch.object(self.pool, "putconn", put))

    def _observe_database_stages(self):
        owner = self
        original_auth = ProductSessionAuth.authenticate
        original_prepare = streaming._prepare_agent_run_stream
        original_close = DatabaseWrapper.close
        original_cursor = DatabaseWrapper.create_cursor
        session_store = import_module(settings.SESSION_ENGINE).SessionStore
        original_save = session_store.save
        terminal_poll = agent_run_stream._load_terminal_sequence
        original_poll = terminal_poll.func

        def authenticate(auth, request, key):
            token = _lease_stage.set("sessionAuth")
            try:
                return original_auth(auth, request, key)
            finally:
                _lease_stage.reset(token)

        async def prepare(**kwargs):
            token = _lease_stage.set("streamPreparation")
            try:
                return await original_prepare(**kwargs)
            finally:
                _lease_stage.reset(token)

        def save(store, *args, **kwargs):
            token = _lease_stage.set("sessionSave")
            state = owner.requests.get(_lease_request.get())

            def pause(execute, sql, params, many, context):
                result = execute(sql, params, many, context)
                if not state["ormEntered"].is_set():
                    owner._record("ormEntered", connection=id(context["connection"].connection))
                    state["ormEntered"].set()
                    if not state["ormRelease"].wait(8):
                        raise TimeoutError("HTTP disconnect did not release the session-save SQL barrier")
                return result

            try:
                if state and state["mode"] == "session-save-disconnect":
                    with connections["default"].execute_wrapper(pause):
                        return original_save(store, *args, **kwargs)
                return original_save(store, *args, **kwargs)
            finally:
                _lease_stage.reset(token)

        def close(wrapper):
            owner._record("wrapperClose", wrapper=id(wrapper),
                connection=id(wrapper.connection) if wrapper.connection is not None else None)
            return original_close(wrapper)

        def cursor(wrapper, *args, **kwargs):
            result = original_cursor(wrapper, *args, **kwargs)
            owner._record("cursor", wrapper=id(wrapper), pool=id(wrapper.pool),
                connection=id(wrapper.connection))
            state = owner.requests.get(_lease_request.get())
            if state and state["mode"] == "workspace-file-open-disconnect" and not state.get("pauseInstalled"):
                state["pauseInstalled"] = True

                def pause(execute, sql, params, many, context):
                    try:
                        value = execute(sql, params, many, context)
                        owner._record("ormEntered", connection=id(context["connection"].connection))
                        state["ormEntered"].set()
                        if not state["ormRelease"].wait(8):
                            raise TimeoutError("HTTP disconnect did not release authorized file opening SQL")
                        return value
                    finally:
                        context["connection"].execute_wrappers.remove(pause)

                wrapper.execute_wrappers.append(pause)
            return result

        def poll(*args, **kwargs):
            token = _lease_stage.set("terminalPoll")
            state = owner.requests.get(_lease_request.get())

            def pause(execute, sql, params, many, context):
                result = execute(sql, params, many, context)
                if not state["ormEntered"].is_set():
                    owner._record("ormEntered", connection=id(context["connection"].connection))
                    state["ormEntered"].set()
                    if not state["ormRelease"].wait(8):
                        raise TimeoutError("HTTP disconnect did not release the ORM test barrier")
                return result

            try:
                if state and state["mode"] == "orm-disconnect":
                    with connections["default"].execute_wrapper(pause):
                        return original_poll(*args, **kwargs)
                return original_poll(*args, **kwargs)
            finally:
                _lease_stage.reset(token)

        def finished(**_kwargs):
            self._record("requestFinished")

        self.enterContext(patch.object(ProductSessionAuth, "authenticate", authenticate))
        self.enterContext(patch.object(streaming, "_prepare_agent_run_stream", prepare))
        self.enterContext(patch.object(session_store, "save", save))
        self.enterContext(patch.object(DatabaseWrapper, "close", close))
        self.enterContext(patch.object(DatabaseWrapper, "create_cursor", cursor))
        self.enterContext(patch.object(terminal_poll, "func", poll))
        signals.request_finished.connect(finished, weak=False)
        self.addCleanup(signals.request_finished.disconnect, finished)

    def _start_http_server(self):
        from api.asgi import WorkspaceApplication, application
        full_lifespan = getattr(self, "full_lifespan", False)
        if full_lifespan:
            application = WorkspaceApplication()
        owner = self

        async def observed(scope, receive, send):
            request_id = dict(scope.get("headers", [])).get(b"x-lease-test-request", b"").decode()
            state = owner.requests.get(request_id)
            token = _lease_request.set(request_id)

            async def observed_receive():
                message = await receive()
                if state and message["type"] == "http.disconnect":
                    owner._record("transportDisconnect")
                    state["disconnected"].set()
                return message

            async def observed_send(message):
                await send(message)
                if state and message["type"] == "http.response.body":
                    if b'"type":"agent_run_completed"' in message.get("body", b""):
                        state["terminalSent"].set()
                        if state["mode"] == "terminal-disconnect":
                            await asyncio.Event().wait()

            try:
                await application(scope, observed_receive, observed_send)
            finally:
                if state:
                    owner._record("httpRequestFinished")
                    state["finished"].set()
                _lease_request.reset(token)

        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen(128)
        self.port = listener.getsockname()[1]
        self.server = uvicorn.Server(uvicorn.Config(
            observed, host="127.0.0.1", port=self.port, loop="asyncio", http="h11",
            lifespan="on" if full_lifespan else "off", log_level="critical", access_log=False,
            timeout_graceful_shutdown=5,
        ))
        self.server_thread = threading.Thread(
            target=self.server.run, kwargs={"sockets": [listener]}, daemon=True,
        )
        self.server_thread.start()

        def stop_server():
            for state in self.requests.values():
                state["ormRelease"].set()
                if "readRelease" in state:
                    state["readRelease"].set()
            self.server.should_exit = True
            self.server_thread.join(8)
            listener.close()
            self.assertFalse(self.server_thread.is_alive(), "HTTP server did not stop")

        self.addCleanup(stop_server)
        deadline = time.monotonic() + 5
        while not self.server.started and self.server_thread.is_alive() and time.monotonic() < deadline:
            time.sleep(.01)
        self.assertTrue(self.server.started, "real HTTP server did not start")

    def _probe(self):
        client = http.client.HTTPConnection("127.0.0.1", self.port, timeout=3)
        try:
            client.request("GET", "/lease-test/database-probe", headers={
                "X-Internal-Token": settings.INTERNAL_API_TOKEN,
                "X-Lease-Test-Request": "database-probe", "Connection": "close",
            })
            response = client.getresponse()
            body = response.read()
            self.assertEqual(response.status, 200, body[:300])
            self.assertEqual(json.loads(body), {"value": 1, "pid": os.getpid()})
        finally:
            client.close()

    def _assert_pool_idle(self):
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            with self.trace_lock:
                borrowed = dict(self.borrowed)
            stats = self.pool.get_stats()
            if not borrowed and stats["pool_available"] == stats["pool_size"] == 1:
                break
            time.sleep(.01)
        with self.pool._lock:
            waiting = len(self.pool._waiting)
        with self.trace_lock:
            lost = [identity for identity in borrowed if self.physical[identity][0]() is None]
            pids = [pid for reference, pid in self.physical.values() if reference() is not None]
        state = {"stats": stats, "borrowed": borrowed, "lostWithoutPut": lost,
            "waiting": waiting, "events": self.events[-18:]}
        self.assertFalse(borrowed, state)
        self.assertEqual((stats["pool_size"], stats["pool_available"], waiting), (1, 1, 0), state)
        physical = self.observer.execute(
            "SELECT pid,state,xact_start IS NOT NULL FROM pg_stat_activity WHERE pid=ANY(%s)", [pids],
        ).fetchall()
        self.assertEqual(len(physical), 1, state)
        self.assertEqual(physical[0][1:], ("idle", False), physical)

    @staticmethod
    def _read_item(response):
        while True:
            line = response.readline()
            if not line:
                return None
            if line.startswith(b"data: "):
                return json.loads(line[6:])

    def _exercise(self, mode):
        self.assertTrue(settings.SESSION_SAVE_EVERY_REQUEST)
        self.assertIs(DatabaseWrapper._connection_pools["default"], self.pool)
        self.assertEqual(self.pool.max_size, 1)
        self._probe()
        self._assert_pool_idle()
        for number, (session_id, run_id) in enumerate(self.runs[mode]):
            with self.subTest(mode=mode, iteration=number):
                request_id = f"{mode}:{number}"
                state = {name: threading.Event() for name in (
                    "disconnected", "finished", "terminalSent", "ormEntered", "ormRelease",
                )}
                state["mode"] = mode
                self.requests[request_id] = state
                client = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
                response = None
                try:
                    client.request("GET", f"/api/sessions/{session_id}/agent-runs/{run_id}/events", headers={
                        "Cookie": self.cookie, "Accept": "text/event-stream",
                        "X-Lease-Test-Request": request_id,
                    })
                    response = client.getresponse()
                    self.assertEqual(response.status, 200, response.read(300) if response.status != 200 else b"")
                    item = self._read_item(response)
                    self.assertIsNotNone(item)
                    if mode == "orm-disconnect":
                        self.assertTrue(state["ormEntered"].wait(5), "real stream polling ORM was not reached")
                    else:
                        while item["event"]["type"] != "agent_run_completed":
                            item = self._read_item(response)
                            self.assertIsNotNone(item, "public stream ended before terminal")
                        self.assertTrue(state["terminalSent"].wait(5), "server did not acknowledge terminal send")
                    if mode == "normal":
                        self.assertIsNone(self._read_item(response), "normal terminal stream must reach EOF")
                    else:
                        self._record("clientTransportClose", target=request_id)
                        client.sock.shutdown(socket.SHUT_RDWR)
                        client.close()
                        self.assertTrue(state["disconnected"].wait(3), "server did not observe real socket disconnect")
                finally:
                    if response is not None:
                        response.close()
                    client.close()
                    state["ormRelease"].set()
                self.assertTrue(state["finished"].wait(5), "public HTTP lifecycle did not finish")
                self._assert_pool_idle()
                self._probe()
                self._assert_pool_idle()
                with self.trace_lock:
                    queries = [event for event in self.events if event["request"] == request_id and event["event"] == "cursor"]
                    stages = {event["stage"] for event in queries}
                self.assertTrue(queries)
                self.assertEqual({event["pool"] for event in queries}, {id(self.pool)})
                self.assertIn("sessionAuth", stages)
                self.assertIn("streamPreparation", stages)
                self.assertIn("sessionSave", stages)


@override_settings(ROOT_URLCONF=__name__, ALLOWED_HOSTS=["127.0.0.1"])
class PublicStreamHttpDatabaseLeaseTests(HttpDatabaseLeaseFixture, TransactionTestCase):
    def test_public_http_normal_terminal_returns_the_single_pool_lease(self):
        self._exercise("normal")

    def test_public_http_terminal_disconnect_returns_the_single_pool_lease(self):
        self._exercise("terminal-disconnect")

    def test_public_http_orm_disconnect_returns_the_single_pool_lease(self):
        self._exercise("orm-disconnect")

    def test_public_http_session_save_disconnect_returns_the_single_pool_lease(self):
        self.assertTrue(settings.SESSION_SAVE_EVERY_REQUEST)
        self.assertEqual(self.pool.max_size, 1)
        self._probe()
        self._assert_pool_idle()
        for number, (session_id, run_id) in enumerate(self.runs["session-save-disconnect"]):
            with self.subTest(iteration=number):
                request_id = f"session-save-disconnect:{number}"
                state = {name: threading.Event() for name in (
                    "disconnected", "finished", "terminalSent", "ormEntered", "ormRelease",
                )}
                state["mode"] = "session-save-disconnect"
                self.requests[request_id] = state
                client = socket.create_connection(("127.0.0.1", self.port), timeout=5)
                try:
                    # Send a real HTTP request without waiting for response headers:
                    # SessionMiddleware saves while the response is still being prepared.
                    client.sendall((
                        f"GET /api/sessions/{session_id}/agent-runs/{run_id}/events HTTP/1.1\r\n"
                        f"Host: 127.0.0.1:{self.port}\r\nCookie: {self.cookie}\r\n"
                        f"X-Lease-Test-Request: {request_id}\r\nAccept: text/event-stream\r\n\r\n"
                    ).encode())
                    self.assertTrue(state["ormEntered"].wait(5), "real SessionStore.save SQL was not reached")
                    self.assertFalse(state["terminalSent"].is_set(), "disconnect must precede response streaming")
                    self._record("clientTransportClose", target=request_id)
                    client.shutdown(socket.SHUT_RDWR)
                    client.close()
                    self.assertTrue(state["disconnected"].wait(3), "server did not observe the pre-header socket disconnect")
                finally:
                    client.close()
                    state["ormRelease"].set()
                self.assertTrue(state["finished"].wait(5), "public HTTP lifecycle did not finish")
                self._assert_pool_idle()
                self._probe()
                self._assert_pool_idle()
                with self.trace_lock:
                    queries = [event for event in self.events if event["request"] == request_id and event["event"] == "cursor"]
                self.assertEqual({event["pool"] for event in queries}, {id(self.pool)})
                self.assertIn("sessionSave", {event["stage"] for event in queries})


@override_settings(ROOT_URLCONF=__name__, ALLOWED_HOSTS=["127.0.0.1", "testserver"])
class WorkspaceFileHttpDatabaseLeaseTests(HttpDatabaseLeaseFixture, WorkspaceFileHttpFixture, TransactionTestCase):
    """Authorized file opening must not strand a different thread's database lease."""

    full_lifespan = True

    def _prepare_http_fixtures(self):
        self.download_content = b"w" * (4 * 1024 * 1024)
        self.store_files([self.download_content])
        committed, snapshot, _encoded = self.snapshot_upload(document([
            ("lease.txt", self.download_content, False),
        ]))
        self.assertEqual(committed.status_code, 201, committed.content)
        reader = self.new_reader()
        self.download_body = {
            "schema": "runtime.workspace_files.download.v1", **reader,
            "checkpointId": None, "snapshot": snapshot,
            "files": [file_descriptor(self.download_content)],
        }
        self.file_read_state = None
        original_read = FileSequence.read

        def read(sequence, *args, **kwargs):
            value = original_read(sequence, *args, **kwargs)
            state = self.file_read_state
            if state is not None:
                state["reads"] += 1
                pause = ((state["boundary"] == "partial" and state["reads"] == 2)
                    or (state["boundary"] == "content-length" and not value))
                if pause and not state["readEntered"].is_set():
                    state["readEntered"].set()
                    if not state["readRelease"].wait(8):
                        raise TimeoutError("HTTP disconnect did not release the file-read test barrier")
            return value

        self.enterContext(patch.object(FileSequence, "read", read))

    def test_internal_workspace_file_download_returns_the_single_pool_lease(self):
        self.assertEqual(self.pool.max_size, 1)
        self._probe()
        self._assert_pool_idle()
        for number in range(5):
            request_id = f"workspace-file-download:{number}"
            state = {name: threading.Event() for name in (
                "disconnected", "finished", "terminalSent", "ormEntered", "ormRelease",
            )}
            state["mode"] = "workspace-file-download"
            self.requests[request_id] = state
            client = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
            response = None
            try:
                client.request("POST", "/internal/agent-runs/workspace-files/download",
                    body=json.dumps(self.download_body).encode(), headers={
                        "Content-Type": "application/json",
                        "X-Internal-Token": settings.INTERNAL_API_TOKEN,
                        "X-Lease-Test-Request": request_id, "Connection": "close",
                    })
                response = client.getresponse()
                content = bytearray()
                while chunk := response.read(4096):
                    content.extend(chunk)
                self.assertEqual(response.status, 200, content[:300])
                self.assertEqual(bytes(content), self.download_content)
            finally:
                if response is not None:
                    response.close()
                client.close()
            self.assertTrue(state["finished"].wait(5), "download HTTP lifecycle did not finish")
            self._assert_pool_idle()
            self._probe()
            self._assert_pool_idle()
            with self.trace_lock:
                queries = [event for event in self.events if event["request"] == request_id and event["event"] == "cursor"]
            self.assertTrue(queries, "download did not execute its real authorization ORM")
            self.assertEqual({event["pool"] for event in queries}, {id(self.pool)})

    def test_internal_workspace_file_open_disconnect_returns_the_single_pool_lease(self):
        self._probe()
        self._assert_pool_idle()
        for number in range(5):
            request_id = f"workspace-file-open-disconnect:{number}"
            state = {name: threading.Event() for name in (
                "disconnected", "finished", "terminalSent", "ormEntered", "ormRelease",
            )}
            state["mode"] = "workspace-file-open-disconnect"
            self.requests[request_id] = state
            client = socket.create_connection(("127.0.0.1", self.port), timeout=5)
            body = json.dumps(self.download_body).encode()
            try:
                client.sendall((
                    "POST /internal/agent-runs/workspace-files/download HTTP/1.1\r\n"
                    f"Host: 127.0.0.1:{self.port}\r\nContent-Type: application/json\r\n"
                    f"X-Internal-Token: {settings.INTERNAL_API_TOKEN}\r\n"
                    f"X-Lease-Test-Request: {request_id}\r\nContent-Length: {len(body)}\r\n\r\n"
                ).encode() + body)
                self.assertTrue(state["ormEntered"].wait(5), "authorized file opening SQL was not reached")
                self._record("clientTransportClose", target=request_id)
                client.shutdown(socket.SHUT_RDWR)
                client.close()
                self.assertTrue(state["disconnected"].wait(3), "server did not observe download socket disconnect")
            finally:
                client.close()
                state["ormRelease"].set()
            self.assertTrue(state["finished"].wait(5), "download HTTP lifecycle did not finish")
            self._assert_pool_idle()
            self._probe()
            self._assert_pool_idle()

    def test_internal_workspace_file_body_disconnect_returns_the_single_pool_lease(self):
        self._probe()
        self._assert_pool_idle()
        for boundary in ("partial", "content-length"):
            for number in range(5):
                request_id = f"workspace-file-body-disconnect:{boundary}:{number}"
                state = {name: threading.Event() for name in (
                    "disconnected", "finished", "terminalSent", "ormEntered", "ormRelease",
                    "readEntered", "readRelease",
                )}
                state.update(mode="workspace-file-body-disconnect", boundary=boundary, reads=0)
                self.requests[request_id] = state
                self.file_read_state = state
                client = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
                response = None
                try:
                    client.request("POST", "/internal/agent-runs/workspace-files/download",
                        body=json.dumps(self.download_body).encode(), headers={
                            "Content-Type": "application/json", "X-Internal-Token": settings.INTERNAL_API_TOKEN,
                            "X-Lease-Test-Request": request_id,
                        })
                    transport = client.sock
                    response = client.getresponse()
                    self.assertEqual(response.status, 200, response.read(300) if response.status != 200 else b"")
                    self.assertEqual(int(response.getheader("Content-Length")), len(self.download_content))
                    length = settings.STORAGE_STREAM_CHUNK_BYTES if boundary == "partial" else len(self.download_content)
                    self.assertEqual(response.read(length), self.download_content[:length])
                    self.assertTrue(state["readEntered"].wait(5), "non-ORM file-read boundary was not reached")
                    self._record("clientTransportClose", target=request_id, consumedBytes=length)
                    transport.shutdown(socket.SHUT_RDWR)
                    client.close()
                    self.assertTrue(state["disconnected"].wait(3), "server did not observe actual download disconnect")
                finally:
                    if response is not None:
                        response.close()
                    client.close()
                    state["readRelease"].set()
                self.assertTrue(state["finished"].wait(5), "download HTTP lifecycle did not finish")
                self.file_read_state = None
                self._assert_pool_idle()
                self._probe()
                self._assert_pool_idle()

    def test_internal_workspace_file_read_wait_releases_the_single_pool_lease(self):
        self._probe()
        self._assert_pool_idle()
        request_id = "workspace-file-read-wait"
        state = {name: threading.Event() for name in (
            "disconnected", "finished", "terminalSent", "ormEntered", "ormRelease",
            "readEntered", "readRelease",
        )}
        state.update(mode="workspace-file-read-wait", boundary="partial", reads=0)
        self.requests[request_id] = state
        self.file_read_state = state
        client = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        response = None
        transport = None
        try:
            client.request("POST", "/internal/agent-runs/workspace-files/download",
                body=json.dumps(self.download_body).encode(), headers={
                    "Content-Type": "application/json", "X-Internal-Token": settings.INTERNAL_API_TOKEN,
                    "X-Lease-Test-Request": request_id,
                })
            transport = client.sock
            response = client.getresponse()
            self.assertEqual(response.status, 200, response.read(300) if response.status != 200 else b"")
            self.assertEqual(response.read(settings.STORAGE_STREAM_CHUNK_BYTES),
                self.download_content[:settings.STORAGE_STREAM_CHUNK_BYTES])
            self.assertTrue(state["readEntered"].wait(5), "non-ORM file reading did not pause")
            self.assertFalse(state["finished"].is_set(), "probe must run while the download is still open")
            with self.trace_lock:
                pids = [pid for reference, pid in self.physical.values() if reference() is not None]
                borrowed = dict(self.borrowed)
                events = self.events[-18:]
            physical = self.observer.execute(
                "SELECT pid,state,xact_start IS NOT NULL FROM pg_stat_activity WHERE pid=ANY(%s)", [pids],
            ).fetchall()
            facts = {"pool": self.pool.get_stats(), "borrowed": borrowed, "physical": physical,
                "fileReadPaused": state["readEntered"].is_set(), "events": events}
            try:
                self._probe()
            except AssertionError as error:
                error.add_note(json.dumps(facts, default=str))
                raise
        finally:
            if transport is not None:
                transport.shutdown(socket.SHUT_RDWR)
            if response is not None:
                response.close()
            client.close()
            state["readRelease"].set()
            self.file_read_state = None
            self.assertTrue(state["finished"].wait(5), "download HTTP lifecycle did not finish")
        self._assert_pool_idle()
