"""Creation provenance is frozen metadata, independent of the current owner."""
import unittest

from centaeris_e2b_adapter.bridge import Adapter, AdapterConfig
from test_adapter import FakeClient, bound_payload, expected_metadata, request
from test_bulk_transport import direct_fixture_network
from test_control_helpers import control_adapter, setup_detail
from test_sdk_transport_acceptance import provider_fixture


def maintenance_metadata(operation="provision-one"):
    metadata = expected_metadata()
    del metadata["agentRunId"]
    del metadata["processingJobId"]
    return {**metadata, "creationOwnerKind": "maintenanceOperation", "maintenanceOperationId": operation}


class CreationOriginMetadataTests(unittest.TestCase):
    def test_run_origin_pause_preserves_the_original_metadata_without_new_discriminators(self):
        client = FakeClient()
        adapter = Adapter(AdapterConfig(api_key="fixture-key", domain="self.example", lane="cleanup"), client=client)
        response = adapter.handle(request("pause", bound_payload(mode="full")))
        self.assertEqual(response["outcome"], "succeeded", response)
        self.assertEqual(client.sandbox.metadata, expected_metadata())
        self.assertEqual([call[0] for call in client.calls], ["inspect", "pause"])

    def test_maintenance_origin_pause_checks_the_exact_physical_creation_without_a_run_or_job(self):
        client = FakeClient()
        metadata = maintenance_metadata()
        client.sandbox.metadata = dict(metadata)
        adapter = Adapter(AdapterConfig(api_key="fixture-key", domain="self.example", lane="cleanup"), client=client)
        response = adapter.handle(request("pause", bound_payload(mode="full", expectedMetadata=metadata)))
        self.assertEqual(response["outcome"], "succeeded", response)
        self.assertEqual([call[0] for call in client.calls], ["inspect", "pause"])
        self.assertEqual(client.calls[-1][1]["retries"], 0)

    def test_maintenance_origin_attach_uses_only_a_fresh_read_and_preserves_the_provider_deadline(self):
        with direct_fixture_network(), provider_fixture() as fixture:
            metadata = maintenance_metadata()
            detail = setup_detail(fixture, metadata=metadata)
            original_deadline = detail["endAt"]
            response = control_adapter(fixture, lane="ordinary").handle(request("attach", {
                "sandboxId": "sandbox-fixture", "expectedTemplateId": "template-fixture",
                "expectedMetadata": metadata, "workspaceRoot": "/mnt/data", "workspaceUser": "agent"}))
            self.assertEqual(response["outcome"], "succeeded", response)
            self.assertEqual(response["result"]["metadata"], metadata)
            self.assertEqual([(item["method"], item["path"]) for item in fixture.requests],
                             [("GET", "/sandboxes/sandbox-fixture")])
            self.assertEqual(fixture.detail()["endAt"], original_deadline)

    def test_mixed_missing_unknown_and_unsupported_origins_are_rejected_before_provider_access(self):
        base = maintenance_metadata()
        invalid = [base | {"agentRunId": "invented"}, base | {"processingJobId": "invented"},
                   base | {"unexpected": "field"}, base | {"creationOwnerKind": "agentRun"},
                   base | {"creationOwnerKind": "unknown"}, base | {"maintenanceOperationId": ""},
                   base | {"maintenanceOperationId": "invalid operation"},
                   base | {"maintenanceOperationId": "invalid\noperation"},
                   base | {"maintenanceOperationId": "a" * 161},
                   {key: value for key, value in base.items() if key != "creationOwnerKind"},
                   {key: value for key, value in base.items() if key != "maintenanceOperationId"}]
        for metadata in invalid:
            with self.subTest(metadata=metadata):
                client = FakeClient()
                adapter = Adapter(AdapterConfig(api_key="fixture-key", domain="self.example", lane="cleanup"), client=client)
                response = adapter.handle(request("pause", bound_payload(mode="full", expectedMetadata=metadata)))
                self.assertEqual(response["outcome"], "notDispatched", response)
                self.assertEqual(response["error"]["code"], "invalidRequest")
                self.assertEqual(client.calls, [])

    def test_a_different_maintenance_origin_never_reaches_the_pause_effect(self):
        client = FakeClient()
        client.sandbox.metadata = maintenance_metadata("other-operation")
        adapter = Adapter(AdapterConfig(api_key="fixture-key", domain="self.example", lane="cleanup"), client=client)
        response = adapter.handle(request("pause", bound_payload(mode="full", expectedMetadata=maintenance_metadata())))
        self.assertNotEqual(response["outcome"], "succeeded", response)
        self.assertEqual([call[0] for call in client.calls], ["inspect"])
