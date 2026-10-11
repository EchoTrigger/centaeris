"""Full ASGI model requests must return ordinary leases while providers wait.

These tests require PostgreSQL and use real ORM and connection pooling.
The only provider is a local HTTPS server with a temporary, generated certificate.
"""
import asyncio
from copy import deepcopy
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import ipaddress
import json
import os
from pathlib import Path
import secrets
import ssl
import tempfile
import threading

from asgiref.testing import ApplicationCommunicator
from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.x509.oid import NameOID
from django.conf import settings
from django.db import connection, connections, transaction
from django.db.backends.postgresql.base import DatabaseWrapper
from django.http import JsonResponse
from django.test import TransactionTestCase, override_settings
from django.urls import path
import psycopg

from api.urls import urlpatterns as application_patterns
from . import tests as fixtures
from .models import AgentRun, ModelConfig, ModelRunLog, Workspace
from .model_adapter.common import model_database_operation
from .test_model_gateway import configure_gateway


def database_probe(request):
    if not secrets.compare_digest(
        request.headers.get("X-Internal-Token", ""), settings.INTERNAL_API_TOKEN
    ):
        return JsonResponse({"error": "unauthorized"}, status=401)
    with connection.cursor() as cursor:
        cursor.execute("SELECT 1")
        value = cursor.fetchone()[0]
    # Let the real ASGI request_finished lifecycle return this request's lease.
    return JsonResponse({"value": value, "pid": os.getpid()})


urlpatterns = [path("lease-test/database-probe", database_probe), *application_patterns]


def provider_certificate(directory):
    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "model-lease-test")])
    now = datetime.now(timezone.utc)
    cert = (
        x509.CertificateBuilder().subject_name(name).issuer_name(name)
        .public_key(key.public_key()).serial_number(x509.random_serial_number())
        .not_valid_before(now - timedelta(minutes=1))
        .not_valid_after(now + timedelta(hours=1))
        .add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
        .add_extension(x509.SubjectAlternativeName([
            x509.IPAddress(ipaddress.ip_address("127.0.0.1"))
        ]), critical=False)
        .sign(key, hashes.SHA256())
    )
    cert_path, key_path = directory / "provider.crt", directory / "provider.key"
    cert_path.write_bytes(cert.public_bytes(serialization.Encoding.PEM))
    key_path.write_bytes(key.private_bytes(
        serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
        serialization.NoEncryption(),
    ))
    return cert_path, key_path


@override_settings(ROOT_URLCONF=__name__)
class ModelDatabaseLeaseTests(TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        configure_gateway(self)
        self.provider_ready = threading.Event()
        self.provider_release = threading.Event()
        self.provider_requests = []
        self.provider_failures = []
        self.start_provider()
        workspace = Workspace.objects.create(name="Model lease test", createdBy=self.admin)
        workspace.members.add(self.admin)
        session = fixtures.create_session(workspace=workspace, owner=self.admin)
        run = AgentRun.objects.create(
            workspace=workspace, session=session, user=self.admin,
            modelConfig=self.model, prompt="lease-test",
        )
        authorization = fixtures.create_agent_run_authorization(run)
        self.run_id = run.id
        self.body = {
            "schema": fixtures.MODEL_RUN_SCHEMA, "agentRunId": run.id,
            "modelConfigRef": self.model.id,
            "thinkingMode": authorization.payload["thinkingMode"],
            "maxOutputTokens": self.model.maxOutputTokens,
            "authorizationRef": authorization.id,
            "authorizationDigest": authorization.digest,
            "preparedPrompt": fixtures.prepared_prompt_for_test(self.model),
        }
        # Separate physical observation capacity cannot serve ordinary ORM requests.
        self.observer = psycopg.connect(**(connection.get_connection_params() | {
            "application_name": "centaeris-model-lease-test-observer", "connect_timeout": 2,
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

    def start_provider(self):
        directory = tempfile.TemporaryDirectory(prefix="model-lease-test-")
        self.addCleanup(directory.cleanup)
        cert, key = provider_certificate(Path(directory.name))
        previous_cert = os.environ.get("SSL_CERT_FILE")

        def restore_certificate():
            if previous_cert is None:
                os.environ.pop("SSL_CERT_FILE", None)
            else:
                os.environ["SSL_CERT_FILE"] = previous_cert

        self.addCleanup(restore_certificate)
        os.environ["SSL_CERT_FILE"] = str(cert)
        owner = self

        class Provider(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                try:
                    length = int(self.headers.get("Content-Length", "0"))
                    if self.path != "/v1/chat/completions" or not 0 < length <= 128 * 1024:
                        self.send_error(400)
                        return
                    payload = json.loads(self.rfile.read(length))
                    if payload.get("model") != "test":
                        self.send_error(400)
                        return
                    streaming = bool(payload.get("stream"))
                    owner.provider_requests.append(streaming)
                    if streaming:
                        self.send_response(200)
                        self.send_header("Content-Type", "text/event-stream")
                        self.end_headers()
                        chunk = {
                            "id": "lease-test", "object": "chat.completion.chunk",
                            "created": 0, "model": "test", "choices": [{
                                "index": 0, "delta": {"role": "assistant", "content": "lease-test"},
                                "finish_reason": None,
                            }],
                        }
                        self.wfile.write(("data: " + json.dumps(chunk) + "\n\n").encode())
                        self.wfile.flush()
                    owner.provider_ready.set()
                    if not owner.provider_release.wait(15):
                        raise TimeoutError("provider barrier was not released")
                    usage = {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    if streaming:
                        chunk["choices"] = [{"index": 0, "delta": {}, "finish_reason": "stop"}]
                        chunk["usage"] = usage
                        response = ("data: " + json.dumps(chunk) + "\n\ndata: [DONE]\n\n").encode()
                    else:
                        response = json.dumps({
                            "id": "lease-test", "object": "chat.completion", "created": 0,
                            "model": "test", "usage": usage, "choices": [{
                                "index": 0, "finish_reason": "stop", "message": {
                                    "role": "assistant", "content": "lease-test",
                                },
                            }],
                        }).encode()
                        self.send_response(200)
                        self.send_header("Content-Type", "application/json")
                        self.send_header("Content-Length", str(len(response)))
                        self.end_headers()
                    self.wfile.write(response)
                    self.wfile.flush()
                except Exception as error:
                    owner.provider_failures.append(type(error).__name__)

        server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        server.daemon_threads = True
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(cert, key)
        server.socket = context.wrap_socket(server.socket, server_side=True)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()

        def stop_provider():
            self.provider_release.set()
            server.shutdown()
            server.server_close()
            thread.join(3)

        self.addCleanup(stop_provider)
        self.provider.apiBase = f"https://127.0.0.1:{server.server_port}/v1"
        self.provider.save(update_fields=["apiBase", "updatedAt"])
        self.model.resolvedApiBase = self.provider.apiBase
        self.model.save(update_fields=["resolvedApiBase", "updatedAt"])


    async def request(self, path, body=None, streaming=False):
        from api.asgi import application
        raw = b"" if body is None else json.dumps(body).encode()
        headers = [
            (b"host", b"127.0.0.1"), (b"content-type", b"application/json"),
            (b"content-length", str(len(raw)).encode()),
            (b"x-internal-token", settings.INTERNAL_API_TOKEN.encode()),
        ]
        if streaming:
            headers.append((b"accept", b"text/event-stream"))
        communicator = ApplicationCommunicator(application, {
            "type": "http", "asgi": {"version": "3.0"}, "http_version": "1.1",
            "method": "GET" if body is None else "POST", "scheme": "http",
            "path": path, "raw_path": path.encode(), "query_string": b"", "root_path": "",
            "headers": headers, "client": ("127.0.0.1", 10000), "server": ("127.0.0.1", 80),
        })
        try:
            await communicator.send_input({"type": "http.request", "body": raw, "more_body": False})
            start = await communicator.receive_output(timeout=20)
            content = b""
            while True:
                message = await communicator.receive_output(timeout=20)
                content += message.get("body", b"")
                if not message.get("more_body", False):
                    break
            await communicator.wait(timeout=3)
            return start["status"], content
        finally:
            communicator.stop()

    def assert_provider_wait_releases_ordinary_pool(self, streaming):
        async def exercise():
            expected_probe = {"value": 1, "pid": os.getpid()}
            status, content = await self.request("/lease-test/database-probe")
            self.assertEqual(status, 200, "baseline probe fixture failed")
            self.assertEqual(json.loads(content), expected_probe)
            task = asyncio.create_task(self.request("/internal/model-runs", self.body, streaming))
            try:
                reached = await asyncio.to_thread(self.provider_ready.wait, 5)
                early_status = task.result()[0] if task.done() else None
                self.assertTrue(reached, f"provider fixture barrier not reached; HTTP status={early_status}")
                self.assertFalse(task.done())
                probe_status, probe_body = await self.request("/lease-test/database-probe")
                self.assertFalse(self.provider_release.is_set())
                self.assertFalse(task.done(), "ordinary query must finish before the model")
            finally:
                self.provider_release.set()
                status, content = await asyncio.wait_for(task, 20)
            self.assertEqual(status, 200)
            if streaming:
                frames = content.decode().split("\n\n")
                results = [json.loads(frame.split("data: ", 1)[1]) for frame in frames
                           if frame.startswith("event: result\n")]
                self.assertEqual(len(results), 1)
                self.assertEqual(results[0]["text"], "lease-test")
                self.assertIn(b"event: delta\n", content)
                self.assertNotIn(b"event: error\n", content)
            else:
                self.assertEqual(json.loads(content)["text"], "lease-test")
            self.assertEqual(self.provider_requests, [streaming], "one provider attempt is sufficient")
            self.assertEqual(self.provider_failures, [])
            pool = DatabaseWrapper._connection_pools["default"]
            self.assertEqual(pool.get_stats()["pool_available"], 1)
            final_status, final_body = await self.request("/lease-test/database-probe")
            self.assertEqual(final_status, 200)
            self.assertEqual(json.loads(final_body), expected_probe)
            return probe_status, probe_body, expected_probe

        status, content, expected_probe = asyncio.run(exercise())
        log = ModelRunLog.objects.get(agentRunId=self.run_id)
        self.assertEqual(log.status, "success")
        self.assertEqual((log.promptTokens, log.completionTokens, log.totalTokens), (1, 1, 2))
        self.assertEqual(status, 200, "provider wait retained the only ordinary database lease")
        self.assertEqual(json.loads(content), expected_probe)

    def test_nonstream_provider_pause_returns_ordinary_database_lease(self):
        self.assert_provider_wait_releases_ordinary_pool(streaming=False)

    def test_stream_provider_pause_returns_ordinary_database_lease(self):
        self.assert_provider_wait_releases_ordinary_pool(streaming=True)


    def test_stream_disconnect_releases_connections_and_records_cancellation(self):
        async def exercise():
            from api.asgi import application
            raw = json.dumps(self.body).encode()
            communicator = ApplicationCommunicator(application, {
                "type": "http", "asgi": {"version": "3.0"}, "http_version": "1.1",
                "method": "POST", "scheme": "http", "path": "/internal/model-runs",
                "raw_path": b"/internal/model-runs", "query_string": b"", "root_path": "",
                "headers": [
                    (b"host", b"127.0.0.1"), (b"content-type", b"application/json"),
                    (b"content-length", str(len(raw)).encode()),
                    (b"x-internal-token", settings.INTERNAL_API_TOKEN.encode()),
                    (b"accept", b"text/event-stream"),
                ],
                "client": ("127.0.0.1", 10000), "server": ("127.0.0.1", 80),
            })
            try:
                await communicator.send_input({"type": "http.request", "body": raw, "more_body": False})
                start = await communicator.receive_output(timeout=10)
                self.assertEqual(start["status"], 200)
                content = b""
                while b"event: delta\n" not in content:
                    message = await communicator.receive_output(timeout=10)
                    content += message.get("body", b"")
                    self.assertTrue(message.get("more_body", False), "stream ended before provider pause")
                self.assertTrue(await asyncio.to_thread(self.provider_ready.wait, 5))
                self.assertFalse(self.provider_release.is_set())
                await communicator.send_input({"type": "http.disconnect"})
                await communicator.wait(timeout=5)
                self.assertFalse(self.provider_release.is_set(), "disconnect must close ownership without provider completion")
                status, body = await self.request("/lease-test/database-probe")
                self.assertEqual(status, 200)
                self.assertEqual(json.loads(body), {"value": 1, "pid": os.getpid()})
                self.assertEqual(DatabaseWrapper._connection_pools["default"].get_stats()["pool_available"], 1)
            finally:
                self.provider_release.set()
                communicator.stop()

        asyncio.run(exercise())
        log = ModelRunLog.objects.get(agentRunId=self.run_id)
        self.assertEqual(log.status, "error")
        self.assertEqual(log.error, "provider_stream_cancelled")
        self.assertEqual(self.provider_requests, [True])

    def test_model_operation_preserves_caller_owned_transaction_and_rollback(self):
        original_name = ModelConfig.objects.get(pk=self.model.pk).displayName
        with transaction.atomic():
            name = model_database_operation(lambda: ModelConfig.objects.get(pk=self.model.pk).displayName)()
            self.assertEqual(name, original_name)
            ModelConfig.objects.filter(pk=self.model.pk).update(displayName="rolled-back-name")
            transaction.set_rollback(True)
        self.assertEqual(ModelConfig.objects.get(pk=self.model.pk).displayName, original_name)
        self.assertEqual(self.provider_requests, [])
