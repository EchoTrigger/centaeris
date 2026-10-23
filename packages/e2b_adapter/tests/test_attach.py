"""Fresh ordinary attachment through the pinned SDK, without lifecycle mutation."""
import base64
import json
import os
import unittest
from unittest.mock import patch

from centaeris_e2b_adapter.bridge import Adapter, AdapterConfig
from test_adapter import request
from test_bulk_transport import direct_fixture_network
from test_control_helpers import EXPECTED_METADATA, setup_detail
from test_sdk_transport_acceptance import create_request, lifecycle_request, provider_fixture


def attach_request(**extra):
    return request("attach", {"sandboxId": "sandbox-fixture", "expectedTemplateId": "template-fixture",
        "expectedMetadata": dict(EXPECTED_METADATA), "workspaceRoot": "/mnt/data", "workspaceUser": "agent", **extra})


def adapter(api, data=None, lane="ordinary"):
    return Adapter(AdapterConfig(api_key="e2b_" + "a" * 64, api_url=api.api_url,
        sandbox_url=(data or api).api_url, lane=lane))


class FreshAttachTests(unittest.TestCase):
    def test_fresh_attach_uses_get_then_native_files_and_process_on_scoped_data_endpoint(self):
        with direct_fixture_network(), provider_fixture() as api, provider_fixture("fileData") as data, provider_fixture() as decoy:
            detail = setup_detail(api)
            end_at = detail["endAt"]
            with patch.dict(os.environ, {"E2B_API_URL": decoy.api_url, "E2B_SANDBOX_URL": decoy.api_url}):
                bridge = adapter(api, data)
                attached = bridge.handle(attach_request())
                self.assertEqual(attached["outcome"], "succeeded", attached)
                self.assertEqual(attached["result"], {"sandboxId": "sandbox-fixture", "templateId": "template-fixture",
                    "metadata": EXPECTED_METADATA, "state": "running"})
                read = bridge.handle(lifecycle_request("readFile", path="/mnt/data/binary", user="agent"))
                self.assertEqual(read["outcome"], "succeeded", read)
                self.assertEqual(base64.b64decode(read["result"]["dataBase64"]), b"\x00\xffself-hosted-data")
                started = bridge.handle(lifecycle_request("runCommand", command="printf accepted", timeoutSeconds=5, user="agent"))
                self.assertEqual(started["outcome"], "succeeded", started)
                completed = bridge.handle(lifecycle_request("waitCommand", pid=started["result"]["pid"], timeoutSeconds=5))
                self.assertEqual(completed["outcome"], "succeeded", completed)
            self.assertEqual([(r["method"], r["path"]) for r in api.requests], [("GET", "/sandboxes/sandbox-fixture")])
            self.assertTrue(data.requests[0]["path"].startswith("/files?"))
            self.assertEqual((data.requests[1]["method"], data.requests[1]["path"]), ("POST", "/process.Process/Start"))
            self.assertTrue(all("x-api-key" not in r["headers"] for r in data.requests))
            self.assertTrue(all(r["headers"]["x-access-token"] == "fixture-envd-access-token" for r in data.requests))
            self.assertNotIn("fixture-envd-access-token", json.dumps(attached))
            self.assertEqual(api.detail()["endAt"], end_at)
            self.assertEqual(decoy.requests, [])

    def test_unsafe_or_missing_policy_does_not_cache_an_execution_handle(self):
        cases = ({"state": "paused"}, {"lifecycle": None},
            *({"lifecycle": {"onTimeout": "kill", "autoResume": value}} for value in (True, 0, "false")),
            {"metadata": {**EXPECTED_METADATA, "unexpected": "extra"}}, {"envdAccessToken": None},
            {"templateID": "another-template"}, {"sandboxID": "another-sandbox"})
        for case in cases:
            with self.subTest(case=case), direct_fixture_network(), provider_fixture() as api, provider_fixture("fileData") as data:
                setup_detail(api, **case)
                bridge = adapter(api, data)
                result = bridge.handle(attach_request())
                self.assertEqual(result["outcome"], "unknown", result)
                read = bridge.handle(lifecycle_request("readFile", path="/mnt/data/binary"))
                self.assertEqual(read["outcome"], "notDispatched", read)
                self.assertEqual(read["error"]["code"], "connectionRequired")
                self.assertEqual([(r["method"], r["path"]) for r in api.requests], [("GET", "/sandboxes/sandbox-fixture")])
                self.assertEqual(data.requests, [])

    def test_attach_is_ordinary_only_and_strict_before_any_provider_request(self):
        with direct_fixture_network(), provider_fixture() as api:
            setup_detail(api)
            for lane in ("control", "cleanup"):
                self.assertEqual(adapter(api, lane=lane).handle(attach_request())["outcome"], "notDispatched")
            ordinary = adapter(api)
            for payload in (attach_request(workspaceUser="root"), attach_request(workspaceRoot="/tmp"),
                    attach_request(expectedMetadata={}), attach_request(expectedTemplateId="template:latest"),
                    attach_request(timeoutSeconds=300), attach_request(onResume="restore"), attach_request(command="unexpected")):
                self.assertEqual(ordinary.handle(payload)["outcome"], "notDispatched")
            self.assertEqual(api.requests, [])

    def test_attachment_does_not_recover_or_reexecute_lost_original_output(self):
        with direct_fixture_network(), provider_fixture() as api, provider_fixture("fileData") as data:
            setup_detail(api)
            first = adapter(api, data)
            self.assertEqual(first.handle(create_request())["outcome"], "succeeded")
            started = first.handle(lifecycle_request("runCommand", command="original", timeoutSeconds=5))
            self.assertEqual(started["outcome"], "succeeded", started)
            fresh = adapter(api, data)
            self.assertEqual(fresh.handle(attach_request())["outcome"], "succeeded")
            missing = fresh.handle(lifecycle_request("recoverCommand", pid=started["result"]["pid"], timeoutSeconds=5))
            self.assertEqual(missing["outcome"], "unknown", missing)
            self.assertEqual(missing["error"]["code"], "commandOutputUnavailable")
            self.assertEqual(first.handle(attach_request())["outcome"], "succeeded")
            original = first.handle(lifecycle_request("recoverCommand", pid=started["result"]["pid"], timeoutSeconds=5))
            self.assertEqual(original["outcome"], "succeeded", original)
            self.assertEqual(sum(r["path"] == "/process.Process/Start" for r in data.requests), 1)
            self.assertEqual(sum(r["path"] == "/process.Process/Connect" for r in data.requests), 0)
            self.assertEqual(sum(r["method"] == "POST" for r in api.requests), 1)

    def test_lost_inspection_reply_stays_unknown_without_retry_or_fallback(self):
        with direct_fixture_network(), provider_fixture("lostInspectionReply") as api, provider_fixture("fileData") as data:
            setup_detail(api)
            result = adapter(api, data).handle(attach_request())
            self.assertEqual(result["outcome"], "unknown", result)
            self.assertEqual(len(api.requests), 1)
            self.assertEqual(data.requests, [])