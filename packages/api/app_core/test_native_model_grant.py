from base64 import urlsafe_b64decode
import hashlib
import hmac
import json
from unittest.mock import AsyncMock, patch

from cryptography.exceptions import InvalidTag
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from django.conf import settings
from django.contrib.auth import get_user_model
from django.db import connection
from django.http import HttpResponse
from django.test import SimpleTestCase, TransactionTestCase, override_settings

from .agent_run_authorization_factory import create_agent_run_authorization
from .credentials import encrypt_credential_secret
from .environment_lifecycle import acquire_environment_controller
from .http import internal_model
from .models import AgentRun, ModelConfig, ModelProvider, ProviderCredential, Workspace
from .testing import create_session, leased_lifecycle_job_fixture


PATH = "/internal/model-runs/native-grant"


def signed_request(**overrides):
    body = {
        "schema": "runtime.native_model_grant.request.v1",
        "agentRunId": "run_fixture",
        "authorizationRef": "grant_fixture",
        "authorizationDigest": "sha256:" + "1" * 64,
        "leaseOwner": "worker:fixture:run_fixture",
        "providerId": "provider_fixture",
    } | overrides
    message = (
        f"workspace:native-model-grant:v1\0{body['agentRunId']}\0{body['authorizationRef']}\0"
        f"{body['authorizationDigest']}\0{body['leaseOwner']}\0{body['providerId']}"
    ).encode("utf-8")
    body["proof"] = "hmac-sha256:" + hmac.new(
        settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY.encode(), message, hashlib.sha256,
    ).hexdigest()
    return body


def open_grant(envelope, proof):
    aad = proof.encode("utf-8")
    reply_key = hmac.new(
        settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY.encode("utf-8"),
        b"workspace:native-model-grant:reply:v1\0" + aad,
        hashlib.sha256,
    ).digest()
    nonce, ciphertext = (
        urlsafe_b64decode(value + "=" * (-len(value) % 4))
        for value in (envelope["nonce"], envelope["ciphertext"])
    )
    return json.loads(AESGCM(reply_key).decrypt(nonce, ciphertext, aad))


class NativeModelGrantRequestTests(SimpleTestCase):
    def test_requires_internal_token_and_bound_runtime_proof_before_authority_lookup(self):
        body = signed_request()
        with patch.object(internal_model, "_resolve_native_model_grant", new_callable=AsyncMock) as resolve:
            resolve.return_value = HttpResponse(status=204)
            self.assertEqual(self.client.post(PATH, body, content_type="application/json").status_code, 401)
            for change, status in (
                ({"providerId": "different_provider"}, 403),
                ({"leaseOwner": "x" * 161}, 400),
                ({"unexpected": True}, 400),
            ):
                with self.subTest(change=change):
                    response = self.client.post(
                        PATH, body | change, content_type="application/json",
                        HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
                    )
                    self.assertEqual(response.status_code, status)
            resolve.assert_not_awaited()
            response = self.client.post(
                PATH, body, content_type="application/json", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
            )
            self.assertEqual(response.status_code, 204)
            self.assertEqual(response.headers["Cache-Control"], "no-store")
            resolve.assert_awaited_once()


@override_settings(MODEL_PROVIDER_TIMEOUT_SECONDS=17)
class NativeModelGrantAuthorityTests(TransactionTestCase):
    def setUp(self):
        self.user = get_user_model().objects.create_user(username="native-model-fixture")
        self.workspace = Workspace.objects.create(name="Native model fixture", createdBy=self.user)
        self.workspace.members.add(self.user)
        self.provider = ModelProvider.objects.create(
            workspace=self.workspace, displayName="Fixture", api="openai-responses",
            apiBase="https://models.example.invalid/v1",
        )
        self.credential = ProviderCredential.objects.create(
            provider=self.provider, displayName="Fixture credential",
            encryptedSecret=encrypt_credential_secret("synthetic-native-bearer"),
            createdBy=self.user, updatedBy=self.user,
        )
        self.model = ModelConfig.objects.create(
            provider=self.provider, modelName="fixture-model", maxOutputTokens=1024,
        )
        session = create_session(workspace=self.workspace, owner=self.user)
        self.run = AgentRun.objects.create(
            workspace=self.workspace, session=session, user=self.user, modelConfig=self.model, prompt="fixture",
        )
        self.authorization = create_agent_run_authorization(self.run, image_digest="sha256:" + "0" * 64)
        self.lease_owner = leased_lifecycle_job_fixture(self, self.run)
        with connection.cursor() as cursor:
            cursor.execute(
                "CREATE TABLE runtime.runtime_events(event_id text PRIMARY KEY, session_id text, task_id text, "
                "event_type text NOT NULL, visibility text, at_ms bigint, payload_json text)"
            )
        acquire_environment_controller(self.run.id, self.lease_owner)

    def grant_request(self, **overrides):
        return signed_request(**({
            "agentRunId": self.run.id,
            "authorizationRef": self.authorization.id,
            "authorizationDigest": self.authorization.digest,
            "leaseOwner": self.lease_owner,
            "providerId": self.provider.id,
        } | overrides))

    def grant(self, **overrides):
        return self.client.post(
            PATH, self.grant_request(**overrides), content_type="application/json",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )

    def test_rotation_changes_bound_bearer_and_authority_witness(self):
        response = self.grant()
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.headers["Cache-Control"], "no-store")
        envelope = response.json()
        self.assertEqual(set(envelope), {"schema", "nonce", "ciphertext"})
        self.assertEqual(envelope["schema"], "workspace.native_model_grant.sealed.v1")
        self.assertRegex(envelope["nonce"], r"^[A-Za-z0-9_-]{16}$")
        self.assertRegex(envelope["ciphertext"], r"^[A-Za-z0-9_-]+$")
        self.assertNotIn(b"synthetic-native-bearer", response.content)
        with self.assertRaises(InvalidTag):
            open_grant(envelope, self.grant_request(providerId="different_provider")["proof"])
        proof = self.grant_request()["proof"]
        first = open_grant(envelope, proof)
        self.assertRegex(first["authorityDigest"], r"^sha256:[0-9a-f]{64}$")
        self.assertEqual(first, {
            "schema": "workspace.native_model_grant.v1",
            "authorityDigest": first["authorityDigest"],
            "apiBase": self.provider.apiBase,
            "model": self.model.modelName,
            "maxOutputTokens": 1024,
            "thinkingMode": None,
            "providerTimeoutMs": 17_000,
            "bearerToken": "synthetic-native-bearer",
        })
        self.credential.encryptedSecret = encrypt_credential_secret("rotated-synthetic-bearer")
        self.credential.version += 1
        self.credential.save(update_fields=["encryptedSecret", "version"])
        response = self.grant()
        self.assertEqual(response.status_code, 200)
        rotated_envelope = response.json()
        self.assertNotEqual(rotated_envelope["nonce"], envelope["nonce"])
        self.assertNotIn(b"rotated-synthetic-bearer", response.content)
        second = open_grant(rotated_envelope, proof)
        self.assertEqual(second["bearerToken"], "rotated-synthetic-bearer")
        self.assertNotEqual(second["authorityDigest"], first["authorityDigest"])

    def test_only_valid_unselected_route_can_return_fallback(self):
        self.assertEqual(self.grant(providerId="different_provider").status_code, 204)
        self.assertEqual(self.grant(leaseOwner="worker:fixture:wrong-owner").status_code, 409)
        with connection.cursor() as cursor:
            cursor.execute("UPDATE runtime.runtime_jobs SET lease_expires_at_ms=0")
        response = self.grant()
        self.assertEqual(response.status_code, 409)
        self.assertEqual(response.json(), {"error": "native_model_authority_denied"})
