from datetime import timedelta
import json

from django.test import Client, TestCase
from django.utils import timezone

from app_core.app_delegations import token_digest
from app_core.models import AgentRun, BusinessApplication, UserAppDelegation
from app_core.test_app_delegations import ALL_SCOPES
from app_core.test_assistant_connectors import AssistantConnectorFixture


class DelegatedConnectorTests(AssistantConnectorFixture, TestCase):
    def setUp(self):
        super().setUp()
        self.ready_binding()
        self.app = BusinessApplication.objects.create(name="Fixture app", status="active", created_by=self.custodian)
        self.grant = UserAppDelegation.objects.create(user=self.member, app=self.app, workspace=self.workspace,
            definition_id=self.definition["id"], membership_ref=self.membership.id, scopes=ALL_SCOPES,
            token_digest=token_digest("cwa_" + "a" * 43), expires_at=timezone.now() + timedelta(hours=1))
        self.client.force_login(self.member)
        agent = self.instance(self.definition).json()["agent"]
        accepted = Client().post(self.base + "/sessions/new/messages", json.dumps({
            "operationId": "delegated-connector", "agentId": agent["id"],
            "text": "fixture", "modelConfigRef": self.model.id,
        }), content_type="application/json", HTTP_AUTHORIZATION="Bearer cwa_" + "a" * 43)
        self.assertEqual(accepted.status_code, 202, accepted.content)
        self.run_record = AgentRun.objects.get(id=accepted.json()["agentRunId"])
        self.assertEqual(self.run_record.app_delegation_id, self.grant.id)

    def test_cached_dispatch_rechecks_originating_delegation(self):
        connected = self.authorize(self.run_record)
        self.assertEqual(connected.status_code, 200, connected.content)
        self.grant.revoked_at = timezone.now()
        self.grant.save(update_fields=["revoked_at"])
        denied = self.authorize(self.run_record, operation="dispatch", bindingDigest=connected.json()["bindingDigest"])
        self.assertEqual(denied.status_code, 403, denied.content)
        self.assertEqual(denied.json(), {"error": "delegation_not_available"})

    def test_app_revocation_and_expiry_block_new_connect_without_changing_snapshot(self):
        snapshot = self.run_record.authorization.payload
        UserAppDelegation.objects.filter(id=self.grant.id).update(expires_at=timezone.now() - timedelta(seconds=1))
        self.assertEqual(self.authorize(self.run_record).status_code, 403)
        UserAppDelegation.objects.filter(id=self.grant.id).update(expires_at=timezone.now() + timedelta(hours=1))
        self.app.status = "revoked"
        self.app.save(update_fields=["status", "updated_at"])
        self.assertEqual(self.authorize(self.run_record).status_code, 403)
        self.assertEqual(self.run_record.authorization.payload, snapshot)

    def test_delegated_run_cannot_drop_or_change_its_durable_application_origin(self):
        self.run_record.acting_app = None
        self.run_record.app_delegation = None
        with self.assertRaisesMessage(ValueError, "application origin is immutable"):
            self.run_record.save()
        self.run_record.refresh_from_db()
        self.assertEqual((self.run_record.acting_app_id, self.run_record.app_delegation_id), (self.app.id, self.grant.id))
