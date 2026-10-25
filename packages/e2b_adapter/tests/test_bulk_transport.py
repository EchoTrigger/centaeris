"""Private spool behavior and real pinned-SDK streamed bulk transfers."""

import base64
from contextlib import contextmanager
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
from pathlib import Path
import stat
import tempfile
import threading
import unittest
from unittest.mock import patch
from types import SimpleNamespace

from centaeris_e2b_adapter.bridge import Adapter, AdapterConfig, serve
from test_adapter import FakeClient, FileReader, bound_payload, request
from test_sdk_transport_acceptance import create_request, provider_fixture

TRANSFER_ID = "123e4567-e89b-12d3-a456-426614174001"
BINARY = bytes(range(256)) * 1537


def upload_request(data=BINARY, **extra):
    return request("uploadFile", {"sandboxId": "sandbox-a", "path": "/tmp/staged-request", "transferId": TRANSFER_ID,
        "byteLength": len(data), "sha256": hashlib.sha256(data).hexdigest(), **extra})


def download_request(**extra):
    return request("downloadFile", {"sandboxId": "sandbox-a", "path": "/tmp/staged-response", "transferId": TRANSFER_ID,
        "maxBytes": len(BINARY), **extra})


@contextmanager
def direct_fixture_network():
    # These real SDK tests exercise direct loopback IO, independently of the
    # host's proxy configuration and any proxy's request-body buffering.
    proxy_keys = ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy")
    with patch.dict(os.environ, {**{key: "" for key in proxy_keys}, "NO_PROXY": "*", "no_proxy": "*"}):
        yield


@contextmanager
def bulk_data_fixture(data=BINARY, *, disconnect=False):
    received = []

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_):
            pass

        def do_POST(self):
            if self.headers.get("Transfer-Encoding", "").lower() == "chunked":
                chunks = []
                while True:
                    line = self.rfile.readline()
                    if not line:
                        return
                    size = int(line.split(b";", 1)[0], 16)
                    if not size:
                        self.rfile.readline()
                        break
                    chunks.append(self.rfile.read(size))
                    self.rfile.read(2)
                body = b"".join(chunks)
            else:
                body = self.rfile.read(int(self.headers["Content-Length"]))
            if self.headers.get_content_type() == "multipart/form-data":
                boundary = self.headers.get_param("boundary").encode()
                body = body.split(b"\r\n\r\n", 1)[1].rsplit(b"\r\n--" + boundary, 1)[0]
            received.append(body)
            result = json.dumps([{"name": "staged-request", "path": "/tmp/staged-request", "type": "file"}]).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(result)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(result)
            self.close_connection = True

        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Connection", "close")
            self.end_headers()
            content = data[:100000] if disconnect else data
            for offset in range(0, len(content), 65536):
                self.wfile.write(content[offset:offset + 65536])
            self.close_connection = True

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    thread = threading.Thread(target=lambda: server.serve_forever(poll_interval=0.01), daemon=True)
    thread.start()
    try:
        yield "http://127.0.0.1:" + str(server.server_address[1]), received
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


class BulkTransferTests(unittest.TestCase):
    def test_missing_download_is_terminal_but_upload_and_transport_errors_remain_unknown(self):
        try:
            from e2b.exceptions import FileNotFoundException, TimeoutException
        except ImportError:
            self.skipTest("requires the official E2B SDK exception types")
        for error_type, expected in ((FileNotFoundException, "failed"), (TimeoutException, "unknown")):
            with self.subTest(error=error_type.__name__):
                adapter = self.adapter()
                def fail(*args, **kwargs):
                    raise error_type("private data token=secret")
                self.client.sandbox.files.read = fail
                result = adapter.handle(download_request())
                self.assertEqual(result["outcome"], expected)
                self.assertEqual(result["result"], {"sandboxId":"sandbox-a", "transferId":TRANSFER_ID})
                self.assertNotIn("secret", str(result))
                self.assertFalse(any(self.root.iterdir()))
        adapter = self.adapter()
        self.source()
        def write_fail(*args, **kwargs):
            raise FileNotFoundException("private data token=secret")
        self.client.sandbox.files.write = write_fail
        result = adapter.handle(upload_request())
        self.assertEqual(result["outcome"], "unknown")

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name).resolve(strict=True)
        self.client = FakeClient()

    def adapter(self, *, lane="ordinary", enabled=True):
        adapter = Adapter(AdapterConfig(api_key="fixture-key", domain="fixture.invalid", lane=lane,
            spool_root=str(self.root) if enabled else None), client=self.client)
        if lane == "ordinary":
            self.assertEqual(adapter.handle(request("connect", bound_payload(timeoutSeconds=120, onResume="restore")))["outcome"], "succeeded")
        return adapter

    def source(self, data=BINARY):
        target = self.root / (TRANSFER_ID + ".upload")
        target.write_bytes(data)
        return target

    def test_disabled_bulk_and_nonordinary_lanes_have_no_sdk_effects(self):
        for lane, enabled in (("ordinary", False), ("control", True), ("cleanup", True)):
            with self.subTest(lane=lane, enabled=enabled):
                adapter = self.adapter(lane=lane, enabled=enabled)
                before = len(self.client.sandbox.calls)
                for value in (upload_request(), download_request()):
                    response = adapter.handle(value)
                    self.assertEqual(response["outcome"], "notDispatched", response)
                self.assertEqual(len(self.client.sandbox.calls), before)
                self.assertEqual(list(self.root.iterdir()), [])

    def test_spool_configuration_rejects_relative_missing_and_regular_roots(self):
        file = self.root / "not-directory"
        file.write_bytes(b"preserve")
        for root in ("relative", str(self.root / "missing"), str(file)):
            with self.subTest(root=root), self.assertRaises(ValueError):
                Adapter(AdapterConfig(api_key="fixture-key", domain="fixture.invalid", spool_root=root), client=self.client)
        self.assertEqual(self.client.calls, [])
        self.assertEqual(file.read_bytes(), b"preserve")

    def test_upload_size_mismatch_fails_before_sdk(self):
        adapter = self.adapter()
        self.source(b"short")
        before = len(self.client.sandbox.calls)
        response = adapter.handle(upload_request())
        self.assertEqual(response["outcome"], "notDispatched", response)
        self.assertEqual(len(self.client.sandbox.calls), before)

    def test_transfer_ids_cannot_select_an_arbitrary_host_path(self):
        adapter = self.adapter()
        before = len(self.client.sandbox.calls)
        for value in ("../escape", "C:/escape", TRANSFER_ID.upper(), "latest", TRANSFER_ID + ".upload"):
            with self.subTest(transfer_id=value):
                self.assertEqual(adapter.handle(upload_request(transferId=value))["outcome"], "notDispatched")
                self.assertEqual(adapter.handle(download_request(transferId=value))["outcome"], "notDispatched")
        arbitrary = download_request()
        arbitrary["payload"]["localPath"] = str(self.root / "escape")
        self.assertEqual(adapter.handle(arbitrary)["outcome"], "notDispatched")
        self.assertEqual(len(self.client.sandbox.calls), before)
        self.assertEqual(list(self.root.iterdir()), [])

    def test_nonregular_and_preexisting_download_are_rejected_before_sdk(self):
        adapter = self.adapter()
        (self.root / (TRANSFER_ID + ".upload")).mkdir()
        existing = self.root / (TRANSFER_ID + ".download")
        existing.write_bytes(b"preserve")
        before = len(self.client.sandbox.calls)
        self.assertEqual(adapter.handle(upload_request())["outcome"], "notDispatched")
        self.assertEqual(adapter.handle(download_request())["outcome"], "notDispatched")
        self.assertEqual(len(self.client.sandbox.calls), before)
        self.assertEqual(existing.read_bytes(), b"preserve")

    def test_invalid_bounds_and_digest_fail_before_sdk(self):
        adapter = self.adapter()
        self.source()
        before = len(self.client.sandbox.calls)
        for extra in ({"byteLength": 128 * 1024 * 1024 + 1}, {"byteLength": -1}, {"sha256": "A" * 64}, {"sha256": "bad"}):
            self.assertEqual(adapter.handle(upload_request(**extra))["outcome"], "notDispatched")
        for maximum in (0, 128 * 1024 * 1024 + 1):
            self.assertEqual(adapter.handle(download_request(maxBytes=maximum))["outcome"], "notDispatched")
        self.assertEqual(len(self.client.sandbox.calls), before)

    def test_link_and_reparse_stat_entries_are_rejected_before_sdk(self):
        adapter = self.adapter()
        source = self.source()
        before = len(self.client.sandbox.calls)
        original = os.lstat
        for kind in ("symlink", "reparse"):
            def linked(path, *args, **kwargs):
                result = original(path, *args, **kwargs)
                if Path(path) == source:
                    values = {name: getattr(result, name) for name in dir(result) if name.startswith("st_")}
                    if kind == "symlink":
                        values["st_mode"] = stat.S_IFLNK | 0o777
                    else:
                        values["st_file_attributes"] = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
                    return type("LinkStat", (), values)()
                return result
            with self.subTest(kind=kind), patch("os.lstat", side_effect=linked):
                self.assertEqual(adapter.handle(upload_request())["outcome"], "notDispatched")
        self.assertEqual(len(self.client.sandbox.calls), before)
        self.assertEqual(source.read_bytes(), BINARY)

    def test_cli_selects_only_the_explicit_host_spool_configuration(self):
        from centaeris_e2b_adapter.__main__ import main
        with patch.dict(os.environ, {"E2B_API_KEY": "fixture-key", "E2B_DOMAIN": "fixture.invalid",
                                     "E2B_ADAPTER_SPOOL_ROOT": str(self.root)}, clear=True), \
                patch("sys.argv", ["adapter", "--lane", "ordinary"]), \
                patch("sys.stdin", SimpleNamespace(buffer=io.BytesIO())), \
                patch("sys.stdout", SimpleNamespace(buffer=io.BytesIO())), \
                patch("centaeris_e2b_adapter.__main__.Adapter", return_value=self.adapter()) as factory, \
                patch("centaeris_e2b_adapter.__main__.serve"):
            self.assertEqual(main(), 0)
        self.assertEqual(factory.call_args.args[0].spool_root, str(self.root))
        self.assertNotIn(str(self.root), repr(factory.call_args.args[0]))
        with patch.dict(os.environ, {"E2B_ADAPTER_SPOOL_ROOT": str(self.root)}):
            adapter = self.adapter(enabled=False)
        self.assertFalse(adapter.handle(request("capabilities"))["result"]["bulkFileTransfer"])

    def test_empty_transfer_preserves_exact_metadata_without_inline_bytes(self):
        adapter = self.adapter()
        self.source(b"")
        self.client.sandbox.files.write = lambda path, data, **kwargs: self.assertEqual(data.read(65536), b"")
        uploaded = adapter.handle(upload_request(data=b""))
        self.assertEqual(uploaded["outcome"], "succeeded", uploaded)
        self.client.sandbox.reader = FileReader([])
        downloaded = adapter.handle(download_request(maxBytes=1))
        self.assertEqual(downloaded["outcome"], "succeeded", downloaded)
        for result in (uploaded, downloaded):
            self.assertEqual(result["result"]["byteLength"], 0)
            self.assertEqual(result["result"]["sha256"], hashlib.sha256(b"").hexdigest())
        self.assertEqual((self.root / (TRANSFER_ID + ".download")).read_bytes(), b"")

    def test_upload_growth_after_admission_is_unknown_without_sending_extra_bytes(self):
        adapter = self.adapter()
        path = self.source()
        sent = [0]
        def write(remote, data, **kwargs):
            with path.open("ab") as append:
                append.write(b"extra")
            while chunk := data.read(65536):
                sent[0] += len(chunk)
        self.client.sandbox.files.write = write
        response = adapter.handle(upload_request())
        self.assertEqual(response["outcome"], "unknown", response)
        self.assertEqual(response["error"]["code"], "transferLengthMismatch")
        self.assertLessEqual(sent[0], len(BINARY))

    def test_upload_streams_once_and_returns_only_integrity_metadata(self):
        adapter = self.adapter()
        source = self.source()
        reads = []
        def write(path, data, **kwargs):
            self.assertIsInstance(data, io.IOBase)
            content = bytearray()
            while True:
                chunk = data.read(1024 * 1024)
                reads.append(len(chunk))
                if not chunk:
                    break
                content.extend(chunk)
            self.assertEqual(bytes(content), BINARY)
        self.client.sandbox.files.write = write
        response = adapter.handle(upload_request())
        self.assertEqual(response["outcome"], "succeeded", response)
        self.assertEqual(response["result"], {"sandboxId": "sandbox-a", "transferId": TRANSFER_ID,
            "byteLength": len(BINARY), "sha256": hashlib.sha256(BINARY).hexdigest()})
        self.assertLessEqual(max(reads), 65536)
        self.assertEqual(sum(reads), len(BINARY))
        self.assertEqual(source.read_bytes(), BINARY)
        self.assertNotIn(str(self.root), json.dumps(response))

    def test_upload_hash_mismatch_and_stream_failure_are_unknown_after_sdk(self):
        adapter = self.adapter()
        self.source()
        effects = []
        def write(path, data, **kwargs):
            effects.append(path)
            while data.read(65536):
                pass
        self.client.sandbox.files.write = write
        response = adapter.handle(upload_request(sha256="0" * 64))
        self.assertEqual(response["outcome"], "unknown", response)
        self.assertEqual(len(effects), 1)
        def broken(path, data, **kwargs):
            effects.append(path)
            data.read(65536)
            raise ConnectionError("private-host-path")
        self.client.sandbox.files.write = broken
        response = adapter.handle(upload_request())
        self.assertEqual(response["outcome"], "unknown", response)
        self.assertNotIn("private-host-path", json.dumps(response))
        self.assertEqual(len(effects), 2)

    def test_failed_download_admission_removes_its_empty_file_before_sdk(self):
        adapter = self.adapter()
        before = len(self.client.sandbox.calls)
        descriptors = []
        def failed_open(fd, *_args, **_kwargs):
            descriptors.append(fd)
            raise OSError("private-host-path")
        try:
            with patch("centaeris_e2b_adapter.bulk.os.fdopen", side_effect=failed_open):
                response = adapter.handle(download_request())
        finally:
            for fd in descriptors:
                try:
                    os.close(fd)
                except OSError:
                    pass
        self.assertEqual(response["outcome"], "notDispatched", response)
        self.assertEqual(len(self.client.sandbox.calls), before)
        self.assertFalse((self.root / (TRANSFER_ID + ".download")).exists())
        self.assertNotIn("private-host-path", json.dumps(response))

    def test_download_enforces_limit_and_removes_only_its_own_partial_file(self):
        adapter = self.adapter()
        self.client.sandbox.reader = FileReader([BINARY[:200000], BINARY[200000:]])
        untouched = self.root / "keep"
        untouched.write_bytes(b"preserve")
        response = adapter.handle(download_request(maxBytes=len(BINARY) - 1))
        self.assertEqual(response["outcome"], "unknown", response)
        self.assertFalse((self.root / (TRANSFER_ID + ".download")).exists())
        self.assertTrue(self.client.sandbox.reader.closed)
        self.assertEqual(untouched.read_bytes(), b"preserve")
        def broken():
            yield BINARY[:100]
            raise ConnectionError("private-host-path")
        self.client.sandbox.reader = FileReader(broken())
        response = adapter.handle(download_request())
        self.assertEqual(response["outcome"], "unknown", response)
        self.assertFalse((self.root / (TRANSFER_ID + ".download")).exists())
        self.assertNotIn("private-host-path", json.dumps(response))

    def test_download_preserves_a_replacement_instead_of_deleting_unowned_data(self):
        adapter = self.adapter()
        target = self.root / (TRANSFER_ID + ".download")
        replace_after_close = [False]
        original = os.lstat
        def swapped():
            yield b"partial"
            replace_after_close[0] = True
            raise ConnectionError()
        def lstat(path, *args, **kwargs):
            if Path(path) == target and replace_after_close[0]:
                replace_after_close[0] = False
                target.rename(self.root / "previous-download")
                target.write_bytes(b"replacement")
            return original(path, *args, **kwargs)
        self.client.sandbox.reader = FileReader(swapped())
        with patch("os.lstat", side_effect=lstat):
            self.assertEqual(adapter.handle(download_request())["outcome"], "unknown")
        self.assertEqual(target.read_bytes(), b"replacement")

    def test_real_sdk_upload_and_download_above_inline_limit_keep_exact_bytes(self):
        with direct_fixture_network(), provider_fixture() as control, bulk_data_fixture() as (data_url, received):
            adapter = Adapter(AdapterConfig(api_key="e2b_" + "a" * 64, api_url=control.api_url,
                sandbox_url=data_url, spool_root=str(self.root)))
            self.assertEqual(adapter.handle(create_request())["outcome"], "succeeded")
            self.source()
            upload = upload_request()
            upload["payload"]["sandboxId"] = "sandbox-fixture"
            uploaded = adapter.handle(upload)
            self.assertEqual(uploaded["outcome"], "succeeded", uploaded)
            download = download_request()
            download["payload"]["sandboxId"] = "sandbox-fixture"
            source = io.BytesIO(json.dumps(download).encode() + b"\n")
            output = io.BytesIO()
            serve(adapter, source, output)
            downloaded = json.loads(output.getvalue())
            self.assertEqual(downloaded["outcome"], "succeeded", downloaded)
            self.assertEqual(received, [BINARY])
            self.assertEqual((self.root / (TRANSFER_ID + ".download")).read_bytes(), BINARY)
            for result in (uploaded, downloaded):
                self.assertEqual(result["result"]["byteLength"], len(BINARY))
                self.assertEqual(result["result"]["sha256"], hashlib.sha256(BINARY).hexdigest())
                self.assertLess(len(json.dumps(result)), 1024)
                self.assertNotIn("dataBase64", result["result"])
                self.assertNotIn(str(self.root), json.dumps(result))

    def test_real_sdk_octet_stream_upload_is_also_lossless(self):
        with direct_fixture_network(), provider_fixture() as control, bulk_data_fixture() as (data_url, received):
            sandbox = control.sandbox
            control.sandbox = lambda: {**sandbox(), "envdVersion": "0.5.7"}
            adapter = Adapter(AdapterConfig(api_key="e2b_" + "a" * 64, api_url=control.api_url,
                sandbox_url=data_url, spool_root=str(self.root)))
            self.assertEqual(adapter.handle(create_request())["outcome"], "succeeded")
            self.source()
            upload = upload_request()
            upload["payload"]["sandboxId"] = "sandbox-fixture"
            result = adapter.handle(upload)
            self.assertEqual(result["outcome"], "succeeded", result)
            self.assertEqual(received, [BINARY])
            self.assertEqual(result["result"]["sha256"], hashlib.sha256(BINARY).hexdigest())
            self.assertLess(len(json.dumps(result)), 1024)

    def test_real_sdk_download_disconnect_is_unknown_and_cleans_partial(self):
        with direct_fixture_network(), provider_fixture() as control, bulk_data_fixture(disconnect=True) as (data_url, _):
            adapter = Adapter(AdapterConfig(api_key="e2b_" + "a" * 64, api_url=control.api_url,
                sandbox_url=data_url, spool_root=str(self.root)))
            self.assertEqual(adapter.handle(create_request())["outcome"], "succeeded")
            value = download_request()
            value["payload"]["sandboxId"] = "sandbox-fixture"
            response = adapter.handle(value)
            self.assertEqual(response["outcome"], "unknown", response)
            self.assertFalse((self.root / (TRANSFER_ID + ".download")).exists())
