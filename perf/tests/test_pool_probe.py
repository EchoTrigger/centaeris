import asyncio
import importlib.util
from pathlib import Path
import unittest


SPEC = importlib.util.spec_from_file_location(
    "pool_probe", Path(__file__).resolve().parents[1] / "harness" / "pool_probe.py")


class PoolProbeTests(unittest.IsolatedAsyncioTestCase):
    def load(self):
        module = importlib.util.module_from_spec(SPEC)
        SPEC.loader.exec_module(module)
        return module

    async def test_http_failure_records_status_without_sensitive_data(self):
        module = self.load()
        records = []
        sent = []

        async def app(scope, receive, send):
            await send({"type": "http.response.start", "status": 500})
            await send({"type": "http.response.body", "body": b"private"})

        probe = module.PoolProbe(app, lambda: {"state": "disabled"}, records.append)
        await probe({"type": "http", "method": "GET", "path": "/api/example",
                     "query_string": b"secret=private", "headers": [(b"cookie", b"private")]},
                    None, lambda message: self.append(sent, message))
        self.assertEqual(sent[0]["status"], 500)
        self.assertEqual(records[0]["kind"], "httpError")
        self.assertEqual(records[0]["status"], 500)
        self.assertNotIn("private", str(records))
        self.assertEqual(records[0]["pool"], {"state": "disabled"})
        self.assertTrue(records[0]["requestId"])

    async def append(self, target, value):
        target.append(value)

    async def test_exception_is_typed_and_reraised_without_message(self):
        module = self.load()
        records = []

        async def app(scope, receive, send):
            raise TimeoutError("secret password")

        probe = module.PoolProbe(app, lambda: {"state": "uncreated"}, records.append)
        with self.assertRaises(TimeoutError):
            await probe({"type": "http", "method": "GET", "path": "/api/example"}, None, None)
        self.assertEqual(records[0]["exceptionType"], "builtins.TimeoutError")
        self.assertNotIn("secret", str(records))

    async def test_wrapped_exception_keeps_cause_types_and_request_correlation(self):
        module = self.load()
        records = []

        async def app(scope, receive, send):
            try:
                raise TimeoutError("private connection string")
            except TimeoutError as cause:
                error = RuntimeError("private SQL")
                error.__cause__ = cause
                probe.record_exception(scope["path"], scope["method"], error)
                await send({"type": "http.response.start", "status": 500})

        probe = module.PoolProbe(app, lambda: {"state": "disabled"}, records.append)
        await probe({"type": "http", "method": "POST", "path": "/api/example"}, None,
                    lambda message: self.append([], message))
        self.assertEqual(records[0]["exceptionChain"], ["builtins.RuntimeError", "builtins.TimeoutError"])
        self.assertEqual(records[0]["requestId"], records[1]["requestId"])
        self.assertNotIn("private", str(records))

    async def test_lifespan_samples_and_stops_after_forwarded_shutdown(self):
        module = self.load()
        records = []
        sent = []
        sampled = asyncio.Event()

        def record(value):
            records.append(value)
            if len(records) >= 2:
                sampled.set()

        async def app(scope, receive, send):
            await send({"type": "lifespan.startup.complete"})
            await asyncio.wait_for(sampled.wait(), timeout=5)
            await send({"type": "lifespan.shutdown.complete"})

        probe = module.PoolProbe(app, lambda: {"state": "active", "stats": {"pool_size": 2}},
                                 record, interval=.005)
        await probe({"type": "lifespan"}, None, lambda message: self.append(sent, message))
        count = len(records)
        await asyncio.sleep(.015)
        self.assertEqual(len(records), count)
        self.assertGreaterEqual(count, 2)
        self.assertEqual(sent[-1]["type"], "lifespan.shutdown.complete")
        self.assertTrue(all(record["pid"] > 0 for record in records))

    async def test_snapshot_distinguishes_disabled_uncreated_and_numeric_stats(self):
        module = self.load()

        class Pool:
            def get_stats(self):
                return {"pool_size": 8, "requests_errors": 0, "secret": "private"}

        self.assertEqual(module.pool_snapshot(False, {}), {"state": "disabled"})
        self.assertEqual(module.pool_snapshot(True, {}), {"state": "uncreated"})
        self.assertEqual(module.pool_snapshot(True, {"default": Pool()}),
                         {"state": "active", "stats": {"pool_size": 8, "requests_errors": 0}})


if __name__ == "__main__":
    unittest.main()
