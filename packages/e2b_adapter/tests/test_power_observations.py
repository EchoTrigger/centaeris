"""Opt-in power timing never changes the pinned SDK protocol or dispatch count."""
import io
import json
import math
import os
import sys
import unittest
from unittest.mock import patch

from centaeris_e2b_adapter.bridge import Adapter, AdapterConfig, serve
from test_lifecycle_identity import bound_request, select_fixture
from test_sdk_transport_acceptance import provider_fixture


class BrokenWriter:
    def write(self, _text):
        raise OSError("fixture-private-writer-error")

    def flush(self):
        raise OSError("fixture-private-writer-error")


def adapter_for(fixture, output, *, enabled=True, clock=None):
    return Adapter(AdapterConfig(api_key="e2b_" + "a" * 64, api_url=fixture.api_url,
        sandbox_url=fixture.api_url, lane="cleanup", power_observations=enabled),
        timing_writer=output, timing_clock=clock)


def records(output):
    return [json.loads(line) for line in output.getvalue().splitlines()]


class PowerObservationTests(unittest.TestCase):
    def test_power_response_bytes_are_identical_with_and_without_observations(self):
        for operation in ("pause", "connect"):
            wires = []
            for enabled in (False, True):
                with self.subTest(operation=operation, enabled=enabled), provider_fixture() as fixture:
                    select_fixture(fixture)
                    timing = io.StringIO()
                    adapter = adapter_for(fixture, timing, enabled=enabled)
                    output = io.BytesIO()
                    serve(adapter, io.BytesIO((json.dumps(bound_request(operation)) + "\n").encode()), output)
                    wires.append(output.getvalue())
                    self.assertEqual(len(output.getvalue().splitlines()), 1)
                    self.assertEqual(json.loads(output.getvalue())["outcome"], "succeeded")
                    self.assertEqual(bool(timing.getvalue()), enabled)
            if len(wires) == 2:
                self.assertEqual(wires[0], wires[1])

    def test_power_stages_have_exact_finite_shape_and_existing_request_counts(self):
        for operation, expected in (
            ("pause", ["sdkImport", "sdkClientSetup", "sdkIdentityGet", "sdkPause", "sdkHandle"]),
            ("connect", ["sdkImport", "sdkClientSetup", "sdkIdentityGet", "sdkConnect", "sdkResumeVerifyGet", "sdkHandle"]),
        ):
            with self.subTest(operation=operation), provider_fixture() as fixture:
                select_fixture(fixture)
                output = io.StringIO()
                result = adapter_for(fixture, output).handle(bound_request(operation))
                self.assertEqual(result["outcome"], "succeeded")
                observed = records(output)
                self.assertEqual([row["stage"] for row in observed], expected)
                for index, row in enumerate(observed):
                    self.assertEqual(set(row), {"schema", "stage", "value", "outcome", "requestId", "operation"})
                    self.assertEqual(row["schema"], "runtime.e2b.power_timing.v1")
                    self.assertIn(type(row["value"]), (int, float))
                    self.assertTrue(math.isfinite(row["value"]))
                    self.assertGreaterEqual(row["value"], 0)
                    self.assertEqual(row["outcome"], "ok")
                    self.assertEqual(row["requestId"], None if index < 2 else "rpc-" + operation)
                    self.assertEqual(row["operation"], None if index < 2 else operation)
                self.assertEqual([row["method"] for row in fixture.requests],
                    ["GET", "POST"] if operation == "pause" else ["GET", "POST", "GET"])

    def test_lost_reply_has_error_spans_but_never_extra_requests_or_retries(self):
        for operation in ("pause", "connect"):
            with self.subTest(operation=operation), provider_fixture("lost" + operation.capitalize() + "Reply") as fixture:
                select_fixture(fixture)
                output = io.StringIO()
                result = adapter_for(fixture, output).handle(bound_request(operation))
                self.assertEqual(result["outcome"], "unknown")
                observed = records(output)
                self.assertEqual(observed[-1]["stage"], "sdkHandle")
                self.assertEqual(observed[-1]["outcome"], "error")
                failed_stage = "sdkPause" if operation == "pause" else "sdkConnect"
                self.assertEqual([row["outcome"] for row in observed if row["stage"] == failed_stage], ["error"])
                self.assertEqual([row["method"] for row in fixture.requests], ["GET", "POST"])

    def test_disabled_timing_never_touches_writer_or_clock(self):
        def forbidden_clock():
            raise AssertionError("disabled timing clock was called")
        with provider_fixture() as fixture:
            select_fixture(fixture)
            result = adapter_for(fixture, BrokenWriter(), enabled=False, clock=forbidden_clock).handle(bound_request("pause"))
            self.assertEqual(result["outcome"], "succeeded")
            self.assertEqual([row["method"] for row in fixture.requests], ["GET", "POST"])

    def test_writer_failures_do_not_change_the_provider_result(self):
        for operation in ("pause", "connect"):
            with self.subTest(operation=operation), provider_fixture() as fixture:
                select_fixture(fixture)
                result = adapter_for(fixture, BrokenWriter()).handle(bound_request(operation))
                self.assertEqual(result["outcome"], "succeeded")
                self.assertEqual([row["method"] for row in fixture.requests],
                    ["GET", "POST"] if operation == "pause" else ["GET", "POST", "GET"])

    def test_nonfinite_negative_or_failing_clocks_drop_only_the_observation(self):
        for value in (float("nan"), float("inf"), float("-inf"), "clock-error"):
            with self.subTest(value=value), provider_fixture() as fixture:
                select_fixture(fixture)
                output = io.StringIO()
                def clock():
                    if value == "clock-error":
                        raise RuntimeError("fixture-private-clock-error")
                    return value
                result = adapter_for(fixture, output, clock=clock).handle(bound_request("pause"))
                self.assertEqual(result["outcome"], "succeeded")
                self.assertEqual(output.getvalue(), "")
        with provider_fixture() as fixture:
            select_fixture(fixture)
            output = io.StringIO()
            ticks = iter(range(100, -100, -1))
            result = adapter_for(fixture, output, clock=lambda: next(ticks)).handle(bound_request("pause"))
            self.assertEqual(result["outcome"], "succeeded")
            self.assertEqual(output.getvalue(), "")

    def test_unsafe_request_ids_urls_credentials_and_provider_errors_never_enter_timing(self):
        for identity in ("https://fixture-private.invalid/?token=fixture-secret", "unsafe\nfixture-secret", "x" * 129):
            with self.subTest(identity=identity), provider_fixture("refusedPause503") as fixture:
                select_fixture(fixture)
                output = io.StringIO()
                request = bound_request("pause")
                request["requestId"] = identity
                adapter_for(fixture, output).handle(request)
                text = output.getvalue()
                self.assertNotIn(identity, text)
                self.assertNotIn("fixture-secret", text)
                self.assertNotIn("fixture-private", text)
                self.assertNotIn(fixture.api_url, text)
                self.assertNotIn("e2b_" + "a" * 64, text)
                self.assertTrue(all(row["requestId"] is None for row in records(output)))

    def test_capabilities_handle_is_correlated_without_changing_capabilities_payload(self):
        with provider_fixture() as fixture:
            output = io.StringIO()
            request = {"schema": "runtime.e2b.adapter.request.v1", "requestId": "capabilities-fixture",
                "operation": "capabilities", "payload": {}}
            adapter = adapter_for(fixture, output)
            result = adapter.handle(request)
            self.assertEqual(result["outcome"], "succeeded")
            self.assertNotIn("powerObservations", result["result"])
            self.assertEqual(records(output)[-1] | {"value": 0}, {
                "schema": "runtime.e2b.power_timing.v1", "stage": "sdkHandle", "value": 0,
                "outcome": "ok", "requestId": "capabilities-fixture", "operation": "capabilities"})
            self.assertEqual(fixture.requests, [])

    def test_entry_point_requires_the_exact_opt_in_value(self):
        from centaeris_e2b_adapter import __main__ as entry
        for value in (None, "true", "0", "1"):
            environment = {"E2B_API_KEY": "fixture-key", "E2B_DOMAIN": "fixture.invalid"}
            if value is not None:
                environment["CENTAERIS_E2B_POWER_OBSERVATIONS"] = value
            with self.subTest(value=value), patch.dict(os.environ, environment, clear=True), patch.object(
                    sys, "argv", ["adapter", "--lane", "cleanup"]), patch.object(entry, "Adapter") as adapter, patch.object(entry, "serve"):
                self.assertEqual(entry.main(), 0)
                self.assertIs(adapter.call_args.args[0].power_observations, value == "1")
