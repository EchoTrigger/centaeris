"""SDK boundary durations over local fixtures; never a live-provider acceptance."""
import io
import json
import math
import unittest
from unittest.mock import patch

from centaeris_e2b_adapter.bridge import Adapter, AdapterConfig, serve
from test_power_observations import BrokenWriter
from test_sdk_transport_acceptance import create_request, provider_fixture

SDK_SCHEMA = "runtime.e2b.sdk_timing.v1"


def adapter_for(fixture, output, *, enabled=True, clock=None):
    return Adapter(AdapterConfig(api_key="e2b_" + "a" * 64, api_url=fixture.api_url,
        sandbox_url=fixture.api_url, power_observations=enabled), timing_writer=output, timing_clock=clock)


def records(output):
    return [row for row in map(json.loads, output.getvalue().splitlines()) if row["schema"] == SDK_SCHEMA]


class SdkCreateObservationTests(unittest.TestCase):
    def test_create_and_returned_identity_verification_are_separate_safe_numeric_boundaries(self):
        with provider_fixture() as fixture:
            output = io.StringIO()
            result = adapter_for(fixture, output).handle(create_request())
            self.assertEqual(result["outcome"], "succeeded")
            observed = records(output)
            self.assertEqual([row["stage"] for row in observed], ["sdkCreate", "sdkCreateVerifyGet", "sdkHandle"])
            for row in observed:
                self.assertEqual(set(row), {"schema", "stage", "value", "outcome", "requestId", "operation"})
                self.assertIn(type(row["value"]), (int, float))
                self.assertTrue(math.isfinite(row["value"]))
                self.assertGreaterEqual(row["value"], 0)
                self.assertEqual((row["requestId"], row["operation"], row["outcome"]), ("rpc-attempt-fixture", "create", "ok"))
            self.assertEqual([(row["method"], row["path"]) for row in fixture.requests],
                [("POST", "/v2/sandboxes"), ("GET", "/sandboxes/sandbox-fixture")])
            for private in (fixture.api_url, "e2b_" + "a" * 64, "environment-fixture", "request-key-fixture"):
                self.assertNotIn(private, output.getvalue())

    def test_observations_preserve_wire_bytes_and_uncertain_creation_responsibility_without_retries(self):
        for mode, stages, outcomes, methods in (
                ("success", ["sdkCreate", "sdkCreateVerifyGet", "sdkHandle"], ["ok", "ok", "ok"], ["POST", "GET"]),
                ("lostCreateReply", ["sdkCreate", "sdkHandle"], ["error", "error"], ["POST"]),
                ("refusedSecret", ["sdkCreate", "sdkHandle"], ["error", "error"], ["POST"]),
                ("lostInspectionReply", ["sdkCreate", "sdkCreateVerifyGet", "sdkHandle"], ["ok", "error", "error"], ["POST", "GET"])):
            wires = []
            for enabled in (False, True):
                with self.subTest(mode=mode, enabled=enabled), provider_fixture(mode) as fixture:
                    output, timing = io.BytesIO(), io.StringIO()
                    serve(adapter_for(fixture, timing, enabled=enabled),
                        io.BytesIO((json.dumps(create_request()) + "\n").encode()), output)
                    wires.append(output.getvalue())
                    result = json.loads(output.getvalue())
                    self.assertEqual(result["outcome"], "succeeded" if mode == "success" else "unknown")
                    self.assertEqual(result["result"]["attemptId"], "attempt-fixture")
                    self.assertEqual([row["method"] for row in fixture.requests], methods)
                    if enabled:
                        self.assertEqual([row["stage"] for row in records(timing)], stages)
                        self.assertEqual([row["outcome"] for row in records(timing)], outcomes)
                    else:
                        self.assertEqual(timing.getvalue(), "")
            self.assertEqual(wires[0], wires[1])

    def test_post_create_identity_mismatch_is_timed_and_keeps_reported_sandbox(self):
        with provider_fixture() as fixture:
            detail = fixture.detail
            fixture.detail = lambda: detail() | {"sandboxID": "foreign-sandbox"}
            timing = io.StringIO()
            result = adapter_for(fixture, timing).handle(create_request())
            self.assertEqual(result["outcome"], "unknown")
            self.assertEqual(result["result"]["sandboxId"], "sandbox-fixture")
            self.assertEqual([row["outcome"] for row in records(timing)], ["ok", "error", "error"])
            self.assertEqual([row["method"] for row in fixture.requests], ["POST", "GET"])
            self.assertNotIn("foreign-sandbox", timing.getvalue())

    def test_disabled_broken_writers_and_invalid_clocks_cannot_change_dispatch(self):
        def forbidden():
            raise AssertionError("timing clock disabled")
        def broken():
            raise RuntimeError("private-clock-diagnostic")
        for enabled, writer, clock in ((False, BrokenWriter(), forbidden), (True, BrokenWriter(), None),
                (True, io.StringIO(), broken), (True, io.StringIO(), lambda: float("nan"))):
            with self.subTest(enabled=enabled, clock=clock is None), provider_fixture() as fixture:
                result = adapter_for(fixture, writer, enabled=enabled, clock=clock).handle(create_request())
                self.assertEqual(result["outcome"], "succeeded")
                self.assertEqual([row["method"] for row in fixture.requests], ["POST", "GET"])

    def test_unsafe_correlations_and_unknown_stage_operation_pairs_never_emit_private_text(self):
        from centaeris_e2b_adapter.observations import SdkTiming
        output = io.StringIO()
        timing = SdkTiming(True, writer=output)
        for request_id in ("https://private.invalid/?secret=token", "private\nsecret", "x" * 129):
            request = create_request()
            request["requestId"] = request_id
            with timing.request_context(request).span("sdkCreate"):
                pass
        for stage, operation in (("private-secret", "create"), ("sdkCreate", "runCommand"), ("sdkCreate", "kill")):
            timing.emit(stage, 1, "ok", "rpc-fixture", operation)
        for value in (True, float("nan"), float("inf"), -1, "private-secret"):
            timing.emit("sdkCreate", value, "ok", "rpc-fixture", "create")
        timing.emit("sdkCreate", 1, "private-secret", "rpc-fixture", "create")
        self.assertEqual(output.getvalue(), "")
        timing.emit("sdkCreate", 1, "ok", "rpc-fixture", "create")
        self.assertEqual(records(output), [{"schema": SDK_SCHEMA, "stage": "sdkCreate", "value": 1,
            "outcome": "ok", "requestId": "rpc-fixture", "operation": "create"}])


class SdkExecutionObservationTests(unittest.TestCase):
    def test_command_start_and_real_wait_preserve_bytes_and_do_not_time_cached_receipt_as_sdk_wait(self):
        from test_sdk_transport_acceptance import lifecycle_request
        receipts = []
        for enabled in (False, True):
            with self.subTest(enabled=enabled), provider_fixture("commandRaw") as fixture:
                timing = io.StringIO()
                adapter = adapter_for(fixture, timing, enabled=enabled)
                self.assertEqual(adapter.handle(create_request())["outcome"], "succeeded")
                timing.seek(0)
                timing.truncate()
                started = adapter.handle(lifecycle_request("runCommand", command="printf raw-private-content", timeoutSeconds=10))
                self.assertEqual(started["outcome"], "succeeded")
                completed = adapter.handle(lifecycle_request("waitCommand", pid=71, timeoutSeconds=10))
                recovered = adapter.handle(lifecycle_request("recoverCommand", pid=71, timeoutSeconds=10))
                self.assertEqual(completed["outcome"], "succeeded")
                self.assertEqual(recovered, completed | {"requestId": "rpc-recoverCommand"})
                receipts.append([started, completed, recovered])
                if enabled:
                    self.assertEqual([(row["stage"], row["operation"]) for row in records(timing)], [
                        ("sdkCommandStart", "runCommand"), ("sdkHandle", "runCommand"),
                        ("sdkCommandWait", "waitCommand"), ("sdkHandle", "waitCommand"),
                        ("sdkHandle", "recoverCommand")])
                else:
                    self.assertEqual(timing.getvalue(), "")
                self.assertEqual([row["path"] for row in fixture.requests],
                    ["/v2/sandboxes", "/sandboxes/sandbox-fixture", "/process.Process/Start"])
                self.assertNotIn("raw-private-content", timing.getvalue())
                self.assertNotIn("stdout", timing.getvalue())
        self.assertEqual(receipts[0], receipts[1])

    def test_recovery_real_drain_times_failure_without_a_second_process_start_or_connect(self):
        from test_sdk_transport_acceptance import lifecycle_request
        for mode, outcome in (("commandRaw", "ok"), ("commandRawMissingEnd", "error")):
            with self.subTest(mode=mode), provider_fixture(mode) as fixture:
                timing = io.StringIO()
                adapter = adapter_for(fixture, timing)
                adapter.handle(create_request())
                adapter.handle(lifecycle_request("runCommand", command="printf raw", timeoutSeconds=10))
                timing.seek(0)
                timing.truncate()
                result = adapter.handle(lifecycle_request("recoverCommand", pid=71, timeoutSeconds=10))
                self.assertEqual(result["outcome"], "succeeded" if outcome == "ok" else "unknown")
                self.assertEqual([(row["stage"], row["outcome"]) for row in records(timing)],
                    [("sdkCommandWait", outcome), ("sdkHandle", outcome)])
                self.assertEqual([row["path"] for row in fixture.requests],
                    ["/v2/sandboxes", "/sandboxes/sandbox-fixture", "/process.Process/Start"])

    def test_bulk_sdk_stream_boundaries_preserve_exact_bytes_and_private_spool_lifecycle(self):
        from pathlib import Path
        import tempfile
        from test_bulk_transport import (BINARY, TRANSFER_ID, bulk_data_fixture, direct_fixture_network,
            upload_request, download_request)
        receipts = []
        for enabled in (False, True):
            with self.subTest(enabled=enabled), tempfile.TemporaryDirectory() as directory, direct_fixture_network(), \
                    provider_fixture() as control, bulk_data_fixture() as (data_url, received):
                root = Path(directory).resolve(strict=True)
                timing = io.StringIO()
                adapter = Adapter(AdapterConfig(api_key="e2b_" + "a" * 64, api_url=control.api_url,
                    sandbox_url=data_url, spool_root=str(root), power_observations=enabled), timing_writer=timing)
                self.assertEqual(adapter.handle(create_request())["outcome"], "succeeded")
                timing.seek(0)
                timing.truncate()
                source = root / (TRANSFER_ID + ".upload")
                source.write_bytes(BINARY)
                results = []
                for request in (upload_request(), download_request()):
                    request["payload"]["sandboxId"] = "sandbox-fixture"
                    result = adapter.handle(request)
                    self.assertEqual(result["outcome"], "succeeded")
                    results.append(result)
                receipts.append(results)
                self.assertEqual(received, [BINARY])
                self.assertEqual(source.read_bytes(), BINARY)
                self.assertEqual((root / (TRANSFER_ID + ".download")).read_bytes(), BINARY)
                if enabled:
                    self.assertEqual([(row["stage"], row["operation"]) for row in records(timing)], [
                        ("sdkFileUpload", "uploadFile"), ("sdkHandle", "uploadFile"),
                        ("sdkFileDownload", "downloadFile"), ("sdkHandle", "downloadFile")])
                else:
                    self.assertEqual(timing.getvalue(), "")
                self.assertNotIn(str(root), timing.getvalue())
                self.assertNotIn("sha256", timing.getvalue())
                self.assertNotIn(data_url, timing.getvalue())
                self.assertEqual([row["method"] for row in control.requests], ["POST", "GET"])
        self.assertEqual(receipts[0], receipts[1])

    def test_bulk_download_disconnect_remains_unknown_and_own_partial_is_cleaned(self):
        from pathlib import Path
        import tempfile
        from test_bulk_transport import TRANSFER_ID, bulk_data_fixture, direct_fixture_network, download_request
        with tempfile.TemporaryDirectory() as directory, direct_fixture_network(), provider_fixture() as control, \
                bulk_data_fixture(disconnect=True) as (data_url, received):
            root = Path(directory).resolve(strict=True)
            timing = io.StringIO()
            adapter = Adapter(AdapterConfig(api_key="e2b_" + "a" * 64, api_url=control.api_url,
                sandbox_url=data_url, spool_root=str(root), power_observations=True), timing_writer=timing)
            self.assertEqual(adapter.handle(create_request())["outcome"], "succeeded")
            timing.seek(0)
            timing.truncate()
            request = download_request()
            request["payload"]["sandboxId"] = "sandbox-fixture"
            result = adapter.handle(request)
            self.assertEqual(result["outcome"], "unknown")
            self.assertEqual([(row["stage"], row["outcome"]) for row in records(timing)],
                [("sdkFileDownload", "error"), ("sdkHandle", "error")])
            self.assertFalse((root / (TRANSFER_ID + ".download")).exists())
            self.assertEqual(received, [])
            self.assertNotIn(str(root), timing.getvalue())

    def test_observer_baseexceptions_never_replace_existing_business_result_or_failure(self):
        from test_adapter import FakeClient, create_payload, request
        class ObservationFailure(BaseException):
            pass
        def broken():
            raise ObservationFailure()
        for fail in (False, True):
            client = FakeClient()
            if fail:
                client.failure = RuntimeError("private-provider-diagnostic")
            adapter = Adapter(AdapterConfig(api_key="fixture-key", domain="fixture.invalid", power_observations=True),
                client=client, timing_clock=broken)
            result = adapter.handle(request("create", create_payload()))
            self.assertEqual(result["outcome"], "unknown" if fail else "succeeded")
            self.assertEqual(len(client.calls), 1)
            self.assertEqual(result["result"]["attemptId"], "attempt-a")
