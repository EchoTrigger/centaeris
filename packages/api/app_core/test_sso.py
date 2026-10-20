import importlib.util
import json
import os
from pathlib import Path
import threading
from urllib.parse import parse_qs, urlsplit
from unittest.mock import patch

import requests
from django.contrib.auth import get_user_model
from django.test import Client, TestCase, override_settings


class ExternalLoginTests(TestCase):
    password = "Test-only-current-passphrase!2026"

    def setUp(self):
        self.user = get_user_model().objects.create_user(
            username="sso-owner@example.test", email="sso-owner@example.test", password=self.password,
        )
        self.browser = Client(enforce_csrf_checks=True)
        self.browser.force_login(self.user)

    def post(self, path, body, browser=None):
        browser = browser or self.browser
        csrf = browser.get("/api/csrf").json()["csrfToken"]
        return browser.post(path, json.dumps(body), content_type="application/json", HTTP_X_CSRFTOKEN=csrf)

    def test_disabled_sso_has_no_public_providers_and_preserves_local_login(self):
        response = self.browser.get("/api/sso/providers")
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json(), {"providers": []})
        anonymous = Client(enforce_csrf_checks=True)
        response = self.post("/api/login", {"email": self.user.username, "password": self.password}, anonymous)
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["user"]["id"], str(self.user.pk))

    def test_account_identity_listing_requires_login(self):
        response = Client().get("/api/account/external-identities")
        self.assertEqual(response.status_code, 401)
        self.assertEqual(response.json(), {"error": "authentication_required"})

    def test_unknown_callback_fails_without_reflecting_provider_input(self):
        response = self.browser.get("/api/sso/missing/callback?error_description=untrusted-value")
        self.assertEqual(response.status_code, 302)
        target = urlsplit(response["Location"])
        self.assertEqual(target.path, "/login")
        self.assertNotIn("untrusted-value", response["Location"])
        self.assertEqual(parse_qs(target.query)["ssoError"], ["sso_provider_unavailable"])


@override_settings(WEB_ORIGIN="http://testserver", DEBUG=True, SSO_ALLOW_INSECURE_HTTP=True)
class OAuthBindingFlowTests(ExternalLoginTests):
    @classmethod
    def setUpClass(cls):
        super().setUpClass()
        cls.proxy_environment = patch.dict(os.environ, {"NO_PROXY": "127.0.0.1,localhost,testserver"})
        cls.proxy_environment.start()
        cls.addClassCleanup(cls.proxy_environment.stop)
        path = Path(__file__).resolve().parents[3] / "scripts/workspace/oauth_test_provider.py"
        spec = importlib.util.spec_from_file_location("oauth_fixture", path)
        fixture = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(fixture)
        cls.provider = fixture.OAuthTestServer(("127.0.0.1", 0), client_id="centaeris-test-client",
            client_secret="synthetic-test-only-secret", redirect_uri="http://testserver/api/sso/test-oauth/callback")
        cls.thread = threading.Thread(target=cls.provider.serve_forever, daemon=True)
        cls.thread.start()

    @classmethod
    def tearDownClass(cls):
        cls.provider.shutdown()
        cls.provider.server_close()
        cls.thread.join()
        super().tearDownClass()

    def setUp(self):
        super().setUp()
        origin = "http://127.0.0.1:" + str(self.provider.server_port)
        self.configuration = {
            "providerId": "test-oauth", "displayName": "Test OAuth",
            "clientId": "centaeris-test-client", "clientSecret": "synthetic-test-only-secret",
            "authorizeUrl": origin + "/authorize", "tokenUrl": origin + "/token",
            "userinfoUrl": origin + "/userinfo", "scope": "profile", "subjectField": "sub",
            "displayNameField": "name", "tokenEndpointAuthMethod": "client_secret_basic",
        }

    def configured(self):
        return override_settings(SSO_PROVIDERS=[self.configuration])

    def start_link(self, browser=None, **changes):
        return self.post("/api/account/external-identities/test-oauth/link",
            {"currentPassword": self.password, "returnTo": "/settings/security", **changes}, browser)

    def approve(self, response, *, account="alice", cancel=False):
        self.assertEqual(response.status_code, 200, response.content)
        target = response.json()["authorizationUrl"]
        params = parse_qs(urlsplit(target).query)
        self.assertEqual(params["code_challenge_method"], ["S256"])
        self.assertNotIn("synthetic-test-only-secret", target)
        external = requests.get(target, params={"cancel": "1"} if cancel else {"account": account},
            allow_redirects=False, timeout=5)
        self.assertEqual(external.status_code, 302, external.content)
        url = urlsplit(external.headers["Location"])
        return url.path + "?" + url.query

    def bind(self, browser=None, **kwargs):
        browser = browser or self.browser
        return browser.get(self.approve(self.start_link(browser), **kwargs))

    def test_full_http_binding_and_login_preserve_original_user(self):
        from app_core.models import Workspace, WorkspaceMembership
        workspace = Workspace.objects.create(name="Existing workspace", createdBy=self.user)
        membership = WorkspaceMembership.objects.create(workspace=workspace, user=self.user, role="owner")
        with self.configured():
            self.assertEqual(self.browser.get("/api/sso/providers").json(),
                {"providers": [{"id": "test-oauth", "displayName": "Test OAuth"}]})
            original = self.browser.get("/api/me").json()
            result = self.bind()
            self.assertEqual(result.status_code, 302)
            self.assertIn("ssoNotice=linked", result["Location"])
            identities = self.browser.get("/api/account/external-identities").json()
            self.assertEqual(identities["providers"][0]["identity"]["displayName"], "Test Alice")
            self.assertNotIn("synthetic-test-only-secret", json.dumps(identities))
            self.assertEqual(self.browser.get("/api/me").json(), original)
            self.post("/api/logout", {})
            callback = self.approve(self.post("/api/sso/test-oauth/login", {"returnTo": "/workspaces"}))
            result = self.browser.get(callback)
            self.assertEqual(result["Location"], "http://testserver/workspaces")
            self.assertEqual(self.browser.get("/api/me").json(), original)
            self.assertEqual(get_user_model().objects.count(), 1)
            membership.refresh_from_db()
            self.assertEqual((membership.user_id, membership.role, workspace.createdBy_id),
                (self.user.pk, "owner", self.user.pk))
            self.assertNotIn("access_token", json.dumps(dict(self.browser.session)))

    def test_link_requires_password_csrf_and_exact_request_fields(self):
        with self.configured():
            self.assertEqual(self.start_link(currentPassword="wrong").status_code, 403)
            self.assertEqual(self.start_link(unknown=True).status_code, 400)
            no_csrf = Client(enforce_csrf_checks=True)
            no_csrf.force_login(self.user)
            self.assertEqual(no_csrf.post("/api/account/external-identities/test-oauth/link",
                json.dumps({"currentPassword": self.password}), content_type="application/json").status_code, 403)

    def test_callback_is_one_time_and_bound_to_original_browser(self):
        with self.configured():
            callback = self.approve(self.start_link())
            other = Client()
            other.force_login(self.user)
            self.assertIn("sso_state_invalid", other.get(callback)["Location"])
            self.assertIn("ssoNotice=linked", self.browser.get(callback)["Location"])
            self.assertIn("sso_state_invalid", self.browser.get(callback)["Location"])

    def test_cancelled_and_tampered_callbacks_do_not_bind(self):
        with self.configured():
            self.assertIn("sso_cancelled", self.bind(cancel=True)["Location"])
            callback = self.approve(self.start_link())
            self.assertIn("sso_state_invalid", self.browser.get(callback.replace("state=", "state=wrong"))["Location"])
            identity = self.browser.get("/api/account/external-identities").json()["providers"][0]["identity"]
            self.assertIsNone(identity)

    def test_existing_identity_cannot_be_claimed_by_another_local_user(self):
        with self.configured():
            self.bind()
            other_user = get_user_model().objects.create_user(username="other@example.test", password=self.password)
            other = Client(enforce_csrf_checks=True)
            other.force_login(other_user)
            result = self.bind(other)
            self.assertIn("sso_identity_conflict", result["Location"])
            self.assertIsNone(other.get("/api/account/external-identities").json()["providers"][0]["identity"])

    def test_unbound_external_identity_does_not_create_an_account(self):
        with self.configured():
            anonymous = Client(enforce_csrf_checks=True)
            start = self.post("/api/sso/test-oauth/login", {"returnTo": "/"}, anonymous)
            result = anonymous.get(self.approve(start))
            self.assertIn("sso_identity_unlinked", result["Location"])
            self.assertEqual(anonymous.get("/api/me").status_code, 401)
            self.assertEqual(get_user_model().objects.count(), 1)

    def test_logout_during_link_and_password_change_invalidate_pending_link(self):
        with self.configured():
            callback = self.approve(self.start_link())
            self.post("/api/logout", {})
            self.assertIn("sso_state_invalid", self.browser.get(callback)["Location"])
            self.browser.force_login(self.user)
            callback = self.approve(self.start_link())
            self.user.set_password("Changed-test-only-passphrase!2027")
            self.user.save(update_fields=["password"])
            self.assertIn("sso_state_invalid", self.browser.get(callback)["Location"])

    def test_external_redirects_are_rejected(self):
        with self.configured():
            for value in ["https://untrusted.example/", "//untrusted.example/", "/\\untrusted.example/", "//["]:
                with self.subTest(value=value):
                    self.assertEqual(self.start_link(returnTo=value).status_code, 400)

    def test_expired_attempt_and_disabled_account_cannot_login(self):
        from datetime import timedelta
        from django.utils import timezone
        from app_core.models import SsoAuthorizationAttempt
        with self.configured():
            callback = self.approve(self.start_link())
            SsoAuthorizationAttempt.objects.update(expires_at=timezone.now() - timedelta(seconds=1))
            self.assertIn("sso_state_invalid", self.browser.get(callback)["Location"])
            self.bind()
            self.post("/api/logout", {})
            callback = self.approve(self.post("/api/sso/test-oauth/login", {}))
            self.user.is_active = False
            self.user.save(update_fields=["is_active"])
            self.assertIn("sso_account_unavailable", self.browser.get(callback)["Location"])
            self.assertEqual(self.browser.get("/api/me").status_code, 401)

    def test_unlink_rejects_wrong_password_and_revokes_external_sessions(self):
        with self.configured():
            self.bind()
            identity_id = self.browser.get("/api/account/external-identities").json()["providers"][0]["identity"]["id"]
            external = Client(enforce_csrf_checks=True)
            external.get(self.approve(self.post("/api/sso/test-oauth/login", {}, external)))
            self.assertEqual(external.get("/api/me").status_code, 200)
            path = f"/api/account/external-identities/{identity_id}/unlink"
            self.assertEqual(self.post(path, {"currentPassword": "wrong"}).status_code, 403)
            self.assertEqual(self.post(path, {"currentPassword": self.password}).status_code, 200)
            self.assertEqual(self.browser.get("/api/me").status_code, 200)
            self.assertEqual(external.get("/api/me").status_code, 401)

    def test_changed_provider_registration_does_not_reuse_existing_binding(self):
        with self.configured():
            self.bind()
            self.post("/api/logout", {})
            self.configuration["authorizeUrl"] += "?realm=different"
            callback = self.approve(self.post("/api/sso/test-oauth/login", {}))
            self.assertIn("sso_provider_changed", self.browser.get(callback)["Location"])
            self.assertEqual(self.browser.get("/api/me").status_code, 401)
