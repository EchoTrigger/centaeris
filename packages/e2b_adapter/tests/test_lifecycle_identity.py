"""Identity-bearing pause/connect contracts over the exact pinned SDK and loopback HTTP."""
import json
import unittest

from centaeris_e2b_adapter.control import EXPECTED_METADATA_KEYS
from test_sdk_transport_acceptance import adapter_for, lifecycle_request, provider_fixture


def original_metadata():
    return {key: "original-" + key for key in EXPECTED_METADATA_KEYS}


def bound_request(operation, **extra):
    policy = {"timeoutSeconds": 120, "onResume": "restore"} if operation == "connect" else {"mode": "full"}
    return lifecycle_request(operation, expectedTemplateId="template-fixture",
                             expectedMetadata=original_metadata(), **{**policy, **extra})


def select_fixture(fixture):
    fixture.metadata = original_metadata()


class LifecycleIdentityTests(unittest.TestCase):
    def test_pause_and_connect_require_complete_identity_before_any_provider_request(self):
        for operation in ("pause", "connect"):
            for missing in ("both", "expectedTemplateId", "expectedMetadata", "metadataKey"):
                with self.subTest(operation=operation, missing=missing), provider_fixture() as fixture:
                    select_fixture(fixture)
                    request = bound_request(operation)
                    if missing == "both":
                        request["payload"].pop("expectedTemplateId")
                        request["payload"].pop("expectedMetadata")
                    elif missing == "metadataKey":
                        request["payload"]["expectedMetadata"].pop("profileDigest")
                    else:
                        request["payload"].pop(missing)
                    adapter = adapter_for(fixture, lane="ordinary" if operation == "connect" else "cleanup")
                    result = adapter.handle(request)
                    self.assertEqual(result["outcome"], "notDispatched", result)
                    self.assertEqual(fixture.requests, [])

    def test_provider_identity_mismatch_prevents_pause_or_resume_mutation(self):
        for operation in ("pause", "connect"):
            for field in ("sandboxID", "templateID", "metadata"):
                with self.subTest(operation=operation, field=field), provider_fixture() as fixture:
                    select_fixture(fixture)
                    detail = fixture.detail

                    def foreign():
                        value = detail()
                        value[field] = ({**value[field], "agentRunId": "a-successor-run"}
                                        if field == "metadata" else "foreign-identity")
                        return value

                    fixture.detail = foreign
                    adapter = adapter_for(fixture, lane="ordinary" if operation == "connect" else "cleanup")
                    result = adapter.handle(bound_request(operation))
                    self.assertEqual(result["outcome"], "unknown", result)
                    self.assertEqual(result["result"]["sandboxId"], "sandbox-fixture")
                    self.assertEqual([(r["method"], r["path"]) for r in fixture.requests],
                                     [("GET", "/sandboxes/sandbox-fixture")])

    def test_connect_checks_post_resume_identity_before_remembering_execution_handle(self):
        for field in ("sandboxID", "templateID", "metadata", "state"):
            with self.subTest(field=field), provider_fixture() as fixture:
                select_fixture(fixture)
                detail = fixture.detail

                def replaced():
                    value = detail()
                    if sum(r["method"] == "GET" for r in fixture.requests) > 1:
                        if field == "metadata":
                            value[field] = {**value[field], "profileDigest": "foreign-profile"}
                        else:
                            value[field] = "paused" if field == "state" else "foreign-identity"
                    return value

                fixture.detail = replaced
                adapter = adapter_for(fixture)
                result = adapter.handle(bound_request("connect"))
                self.assertEqual(result["outcome"], "unknown", result)
                self.assertEqual(result["result"]["sandboxId"], "sandbox-fixture")
                unavailable = adapter.handle(lifecycle_request("runCommand", command="printf must-not-start", timeoutSeconds=2))
                self.assertEqual(unavailable["outcome"], "notDispatched", unavailable)
                self.assertEqual(unavailable["error"]["code"], "connectionRequired")
                self.assertEqual([r["method"] for r in fixture.requests], ["GET", "POST", "GET"])

    def test_explicit_pause_reports_requested_mode_and_never_observed_preservation(self):
        for mode, memory in (("full", True), ("filesystem", False)):
            with self.subTest(mode=mode), provider_fixture() as fixture:
                select_fixture(fixture)
                adapter = adapter_for(fixture, lane="cleanup")
                result = adapter.handle(bound_request("pause", mode=mode))
                self.assertEqual(result["outcome"], "succeeded", result)
                self.assertEqual(result["result"], {"sandboxId": "sandbox-fixture", "requestedMode": mode, "changed": True})
                self.assertEqual([r["method"] for r in fixture.requests], ["GET", "POST"])
                self.assertEqual(fixture.requests[1]["body"], {"memory": memory})
                capabilities = lifecycle_request("capabilities")
                capabilities["payload"] = {}
                self.assertFalse(adapter.handle(capabilities)["result"]["fullMemoryPauseVerified"])

    def test_provider_409_conflict_is_not_a_fresh_full_memory_snapshot_receipt(self):
        with provider_fixture("alreadyPaused") as fixture:
            select_fixture(fixture)
            result = adapter_for(fixture, lane="cleanup").handle(bound_request("pause"))
            self.assertEqual(result["outcome"], "succeeded", result)
            self.assertEqual(result["result"], {"sandboxId": "sandbox-fixture", "requestedMode": "full", "changed": False})
            self.assertEqual([r["method"] for r in fixture.requests], ["GET", "POST"])

    def test_lost_or_refused_pause_and_connect_never_retry_or_create(self):
        for operation in ("pause", "connect"):
            for response in ("lost", "503", "429"):
                action = operation.capitalize()
                mode = "lost" + action + "Reply" if response == "lost" else "refused" + action + response
                with self.subTest(operation=operation, response=response), provider_fixture(mode) as fixture:
                    select_fixture(fixture)
                    adapter = adapter_for(fixture, lane="ordinary" if operation == "connect" else "cleanup")
                    result = adapter.handle(bound_request(operation))
                    self.assertEqual(result["outcome"], "unknown", result)
                    self.assertEqual(result["result"]["sandboxId"], "sandbox-fixture")
                    self.assertEqual([r["method"] for r in fixture.requests], ["GET", "POST"])
                    self.assertNotIn("fixture-private", json.dumps(result))

    def test_failed_reconnect_does_not_leave_the_old_execution_handle_usable(self):
        with provider_fixture() as fixture:
            select_fixture(fixture)
            adapter = adapter_for(fixture)
            self.assertEqual(adapter.handle(bound_request("connect"))["outcome"], "succeeded")
            fixture.mode = "lostConnectReply"
            self.assertEqual(adapter.handle(bound_request("connect"))["outcome"], "unknown")
            response = adapter.handle(lifecycle_request("runCommand", command="printf forbidden", timeoutSeconds=2))
            self.assertEqual(response["outcome"], "notDispatched", response)
            self.assertEqual(response["error"]["code"], "connectionRequired")

    def test_identity_bearing_requests_keep_control_lane_unable_to_pause_or_resume(self):
        with provider_fixture() as fixture:
            select_fixture(fixture)
            adapter = adapter_for(fixture, lane="control")
            for operation in ("pause", "connect"):
                result = adapter.handle(bound_request(operation))
                self.assertEqual(result["outcome"], "notDispatched", result)
                self.assertEqual(result["error"]["code"], "laneForbidden")
            self.assertEqual(fixture.requests, [])


    def test_cleanup_can_resume_exact_provider_without_acquiring_execution_capabilities(self):
        with provider_fixture() as fixture:
            select_fixture(fixture)
            adapter = adapter_for(fixture, lane="cleanup")
            resumed = adapter.handle(bound_request("connect"))
            self.assertEqual(resumed["outcome"], "succeeded", resumed)
            self.assertEqual(resumed["result"], {
                "sandboxId": "sandbox-fixture", "templateId": "template-fixture",
                "metadata": original_metadata(), "state": "running"})
            expected_requests = [("GET", "/sandboxes/sandbox-fixture"),
                ("POST", "/v2/sandboxes/sandbox-fixture/connect"),
                ("GET", "/sandboxes/sandbox-fixture")]
            self.assertEqual([(r["method"], r["path"]) for r in fixture.requests], expected_requests)
            forbidden = (
                lifecycle_request("runCommand", command="printf forbidden", timeoutSeconds=2),
                lifecycle_request("readFile", path="/mnt/data/secret"),
                lifecycle_request("writeFile", path="/mnt/data/secret", dataBase64=""),
                lifecycle_request("waitCommand", pid=71, timeoutSeconds=2),
                lifecycle_request("recoverCommand", pid=71, timeoutSeconds=2),
                lifecycle_request("cancelCommand", pid=71),
                lifecycle_request("attach", expectedTemplateId="template-fixture",
                    expectedMetadata=original_metadata(), workspaceRoot="/mnt/data", workspaceUser="agent"),
            )
            for request in forbidden:
                with self.subTest(operation=request["operation"]):
                    denied = adapter.handle(request)
                    self.assertEqual(denied["outcome"], "notDispatched", denied)
                    self.assertEqual(denied["error"]["code"], "laneForbidden")
            self.assertEqual([(r["method"], r["path"]) for r in fixture.requests], expected_requests)

    def test_cleanup_resume_unknown_has_one_dispatch_and_keeps_original_identity(self):
        for mode in ("lostConnectReply", "refusedConnect503", "refusedConnect429"):
            with self.subTest(mode=mode), provider_fixture(mode) as fixture:
                select_fixture(fixture)
                adapter = adapter_for(fixture, lane="cleanup")
                result = adapter.handle(bound_request("connect"))
                self.assertEqual(result["outcome"], "unknown", result)
                self.assertEqual(result["result"], {"sandboxId": "sandbox-fixture"})
                self.assertEqual([(r["method"], r["path"]) for r in fixture.requests],
                    [("GET", "/sandboxes/sandbox-fixture"),
                     ("POST", "/v2/sandboxes/sandbox-fixture/connect")])
                self.assertNotIn("fixture-private", json.dumps(result))


if __name__ == "__main__":
    unittest.main()
