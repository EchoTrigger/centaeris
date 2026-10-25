"""Bounded native SDK directory listing, independent of Core atomic files."""
from types import SimpleNamespace
import unittest

from centaeris_e2b_adapter.bridge import Adapter, AdapterConfig
from test_adapter import FakeClient, bound_payload, request

try:
    from e2b.exceptions import FileNotFoundException, NotFoundException, SandboxNotFoundException, TimeoutException
except ImportError:
    FileNotFoundException = None

class NativeFilesTests(unittest.TestCase):
    @unittest.skipUnless(FileNotFoundException, "requires the official E2B SDK exception types")
    def test_missing_file_is_a_bounded_failure_only_for_read_only_sdk_operations(self):
        for operation, method, extra in (("listDirectory", "list", {"recursive": True, "maxEntries": 100}),
                                         ("readFile", "read", {})):
            for error_type, expected in ((FileNotFoundException, "failed"), (NotFoundException, "unknown"),
                                         (SandboxNotFoundException, "unknown"), (TimeoutException, "unknown"),
                                         (RuntimeError, "unknown")):
                with self.subTest(operation=operation, error=error_type.__name__):
                    bridge, client = self.bridge([])
                    calls = []
                    def fail(*args, **kwargs):
                        calls.append(args)
                        raise error_type("private-path token=secret")
                    setattr(client.sandbox.files, method, fail)
                    result = bridge.handle(request(operation, {"sandboxId": "sandbox-a", "path": "/missing", **extra}))
                    self.assertEqual(result["outcome"], expected, result)
                    self.assertEqual(len(calls), 1)
                    self.assertNotIn("secret", str(result))
                    if expected == "failed":
                        self.assertEqual(result["error"]["code"], "fileNotFound")
                        self.assertEqual(result["result"], {"sandboxId": "sandbox-a"})

    @unittest.skipUnless(FileNotFoundException, "requires the official E2B SDK exception types")
    def test_write_failure_keeps_unknown_responsibility(self):
        bridge, client = self.bridge([])
        def fail(*args, **kwargs):
            raise FileNotFoundException("private-path token=secret")
        client.sandbox.files.write = fail
        result = bridge.handle(request("writeFile", {"sandboxId": "sandbox-a", "path": "/missing", "dataBase64": "YQ=="}))
        self.assertEqual(result["outcome"], "unknown")
        self.assertNotIn("secret", str(result))

    def bridge(self, entries):
        client = FakeClient()
        client.sandbox.files.list = lambda path, **options: client.sandbox.calls.append(("list", {"path":path, **options})) or entries
        bridge = Adapter(AdapterConfig(api_key="fixture-only", domain="fixture.example"), client=client)
        self.assertEqual(bridge.handle(request("connect", bound_payload(timeoutSeconds=300, onResume="restore")))["outcome"], "succeeded")
        return bridge, client

    def entry(self, path, kind="file", size=3):
        return SimpleNamespace(path=path, type=SimpleNamespace(value=kind), size=size)

    def test_native_listing_preserves_original_user_and_sdk_recursive_depth(self):
        bridge, client = self.bridge([self.entry("/logs/verifier/reward.txt"), self.entry("/logs/verifier/nested", "dir", 0)])
        result = bridge.handle(request("listDirectory", {"sandboxId":"sandbox-a", "path":"/logs/verifier",
            "user":"root", "recursive":True, "maxEntries":100}))
        self.assertEqual(result["outcome"], "succeeded", result)
        self.assertEqual(result["result"], {"sandboxId":"sandbox-a", "entries":[
            {"path":"/logs/verifier/reward.txt", "kind":"file", "sizeBytes":3},
            {"path":"/logs/verifier/nested", "kind":"directory", "sizeBytes":0}]})
        call = client.sandbox.calls[-1]
        self.assertEqual(call, ("list", {"path":"/logs/verifier", "user":"root", "depth":None, "request_timeout":30.0}))

    def test_native_listing_rejects_links_outside_paths_and_excess_entries_without_false_success(self):
        for entries in ([self.entry("/logs/link", "symlink")], [self.entry("/other/file")],
                [self.entry("/logs/a"), self.entry("/logs/b")]):
            with self.subTest(entries=entries):
                bridge, _ = self.bridge(entries)
                result = bridge.handle(request("listDirectory", {"sandboxId":"sandbox-a", "path":"/logs",
                    "user":"root", "recursive":True, "maxEntries":1}))
                self.assertEqual(result["outcome"], "unknown", result)
                self.assertNotIn("entries", result.get("result", {}))
        bridge, client = self.bridge([])
        for invalid in (0, True, 10001):
            self.assertEqual(bridge.handle(request("listDirectory", {"sandboxId":"sandbox-a", "path":"/logs",
                "recursive":True, "maxEntries":invalid}))["outcome"], "notDispatched")
        self.assertFalse(any(kind=="list" for kind,_ in client.sandbox.calls))
