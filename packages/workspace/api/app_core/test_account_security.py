import json
from datetime import datetime, timedelta
from urllib.parse import parse_qs, urlsplit
from unittest.mock import patch

from django.contrib.auth import get_user_model
from django.contrib.auth.tokens import default_token_generator
from django.core import mail
from django.test import Client, TestCase
from django.test.utils import override_settings

from app_core.models import PasswordResetMail
from app_core.password_reset import process_next_password_reset_mail


class AccountPasswordTests(TestCase):
    old_password = "Current-Passphrase!2026"
    new_password = "Replacement-Passphrase!2027"

    def setUp(self):
        self.user = get_user_model().objects.create_user(
            username="member@example.test",
            email="member@example.test",
            password=self.old_password,
        )

    @staticmethod
    def _csrf(client):
        return client.get("/api/csrf").json()["csrfToken"]

    @staticmethod
    def _patch(client, payload, csrf_token):
        return client.patch(
            "/api/account/password",
            data=json.dumps(payload),
            content_type="application/json",
            HTTP_X_CSRFTOKEN=csrf_token,
        )

    def test_change_password_keeps_current_session_and_invalidates_other_sessions(self):
        current = Client(enforce_csrf_checks=True)
        other = Client(enforce_csrf_checks=True)
        current.force_login(self.user)
        other.force_login(self.user)
        previous_session_key = current.session.session_key

        response = self._patch(
            current,
            {
                "currentPassword": self.old_password,
                "newPassword": self.new_password,
            },
            self._csrf(current),
        )

        self.assertEqual(response.status_code, 200)
        self.assertNotEqual(current.session.session_key, previous_session_key)
        self.assertEqual(current.get("/api/me").status_code, 200)
        self.assertEqual(other.get("/api/me").status_code, 401)
        self.user.refresh_from_db()
        self.assertFalse(self.user.check_password(self.old_password))
        self.assertTrue(self.user.check_password(self.new_password))

    def test_change_password_rejects_wrong_weak_same_and_invalid_requests(self):
        client = Client(enforce_csrf_checks=True)
        client.force_login(self.user)
        csrf_token = self._csrf(client)

        cases = [
            (
                {"currentPassword": "wrong", "newPassword": self.new_password},
                403,
                "account_current_password_invalid",
            ),
            (
                {"currentPassword": self.old_password, "newPassword": "short"},
                400,
                "account_password_invalid",
            ),
            (
                {"currentPassword": self.old_password, "newPassword": self.old_password},
                409,
                "account_password_unchanged",
            ),
            (
                {"current_password": self.old_password, "newPassword": self.new_password},
                400,
                "account_password_request_invalid",
            ),
            (
                {
                    "currentPassword": self.old_password,
                    "newPassword": self.new_password,
                    "banana": True,
                },
                400,
                "account_password_request_invalid",
            ),
        ]
        for payload, status_code, error_code in cases:
            with self.subTest(error_code=error_code, payload=payload):
                response = self._patch(client, payload, csrf_token)
                self.assertEqual(response.status_code, status_code)
                self.assertEqual(response.json(), {"error": error_code})

        response = client.patch(
            "/api/account/password",
            data="{",
            content_type="application/json",
            HTTP_X_CSRFTOKEN=csrf_token,
        )
        self.assertEqual(response.status_code, 400)
        self.assertEqual(response.json(), {"error": "account_password_request_invalid"})

        self.user.refresh_from_db()
        self.assertTrue(self.user.check_password(self.old_password))

    def test_change_password_requires_session_and_csrf(self):
        anonymous = Client(enforce_csrf_checks=True)
        response = self._patch(
            anonymous,
            {"currentPassword": self.old_password, "newPassword": self.new_password},
            "missing",
        )
        self.assertEqual(response.status_code, 401)
        self.assertEqual(response.json(), {"error": "authentication_required"})

        client = Client(enforce_csrf_checks=True)
        client.force_login(self.user)
        response = self._patch(
            client,
            {"currentPassword": self.old_password, "newPassword": self.new_password},
            "missing",
        )
        self.assertEqual(response.status_code, 403)
        self.assertEqual(response.json(), {"error": "csrf_failed"})


@override_settings(
    PASSWORD_RESET_ENABLED=True,
    PASSWORD_RESET_TIMEOUT=86400,
    EMAIL_BACKEND="django.core.mail.backends.locmem.EmailBackend",
    DEFAULT_FROM_EMAIL="Centaeris <no-reply@example.test>",
    WEB_ORIGIN="https://workspace.example.test",
)
class AccountPasswordResetTests(TestCase):
    old_password = "Current-Passphrase!2026"
    new_password = "Replacement-Passphrase!2027"

    def setUp(self):
        self.user = get_user_model().objects.create_user(
            username="member@example.test",
            email="member@example.test",
            password=self.old_password,
        )

    @staticmethod
    def _csrf(client):
        return client.get("/api/csrf").json()["csrfToken"]

    def _post(self, client, path, payload):
        return client.post(
            path,
            data=json.dumps(payload),
            content_type="application/json",
            HTTP_X_CSRFTOKEN=self._csrf(client),
        )

    def test_request_is_neutral_rate_limited_and_sends_fragment_link(self):
        client = Client(enforce_csrf_checks=True)
        for email in [self.user.email, "missing@example.test", "not-an-email"]:
            response = self._post(
                client,
                "/api/account/password-reset-requests",
                {"email": email},
            )
            self.assertEqual(response.status_code, 202)
            self.assertEqual(response.json(), {"ok": True})

        self._post(
            client,
            "/api/account/password-reset-requests",
            {"email": self.user.email},
        )
        self.assertEqual(PasswordResetMail.objects.filter(status="pending").count(), 1)
        self.assertEqual(
            PasswordResetMail.objects.filter(status="suppressed").count(), 1
        )

        self.assertTrue(process_next_password_reset_mail())
        queued = PasswordResetMail.objects.get(user=self.user)
        self.assertEqual(queued.status, "sent")
        self.assertEqual(queued.attempt_count, 1)
        self.assertEqual(len(mail.outbox), 1)
        reset_link = next(
            word
            for word in mail.outbox[0].body.split()
            if word.startswith("https://workspace.example.test/reset-password#")
        )
        self.assertFalse(urlsplit(reset_link).query)
        reset_values = parse_qs(urlsplit(reset_link).fragment)
        self.assertEqual(set(reset_values), {"uid", "token"})
        self.assertNotIn(reset_values["token"][0], queued.last_error_kind)

    def test_reset_is_one_time_validates_password_and_invalidates_all_sessions(self):
        first_session = Client()
        second_session = Client()
        first_session.force_login(self.user)
        second_session.force_login(self.user)

        request_client = Client(enforce_csrf_checks=True)
        self._post(
            request_client,
            "/api/account/password-reset-requests",
            {"email": self.user.email},
        )
        process_next_password_reset_mail()
        reset_link = next(
            word
            for word in mail.outbox[0].body.split()
            if "/reset-password#" in word
        )
        reset_values = parse_qs(urlsplit(reset_link).fragment)
        payload = {
            "uid": reset_values["uid"][0],
            "token": reset_values["token"][0],
            "newPassword": self.new_password,
        }

        weak = self._post(
            request_client,
            "/api/account/password-resets",
            {**payload, "newPassword": "short"},
        )
        self.assertEqual(weak.status_code, 400)
        self.assertEqual(weak.json(), {"error": "account_password_invalid"})

        response = self._post(
            request_client,
            "/api/account/password-resets",
            payload,
        )
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json(), {"ok": True})
        self.assertEqual(first_session.get("/api/me").status_code, 401)
        self.assertEqual(second_session.get("/api/me").status_code, 401)

        replay = self._post(
            request_client,
            "/api/account/password-resets",
            payload,
        )
        self.assertEqual(replay.status_code, 400)
        self.assertEqual(replay.json(), {"error": "account_password_reset_invalid"})

    def test_reset_contract_and_delivery_failure_are_observable_without_pii(self):
        client = Client(enforce_csrf_checks=True)
        disabled = self.settings(PASSWORD_RESET_ENABLED=False)
        with disabled:
            response = self._post(
                client,
                "/api/account/password-reset-requests",
                {"email": self.user.email},
            )
        self.assertEqual(response.status_code, 503)
        self.assertEqual(
            response.json(), {"error": "account_password_reset_unavailable"}
        )

        invalid = self._post(
            client,
            "/api/account/password-resets",
            {"uid": "banana", "token": "banana", "newPassword": self.new_password},
        )
        self.assertEqual(invalid.status_code, 400)
        self.assertEqual(
            invalid.json(), {"error": "account_password_reset_invalid"}
        )

        malformed = self._post(
            client,
            "/api/account/password-resets",
            {
                "uid": "banana",
                "token": "banana",
                "new_password": self.new_password,
            },
        )
        self.assertEqual(malformed.status_code, 400)
        self.assertEqual(
            malformed.json(), {"error": "account_password_reset_request_invalid"}
        )

        self._post(
            client,
            "/api/account/password-reset-requests",
            {"email": self.user.email},
        )
        with patch(
            "app_core.password_reset.send_mail",
            side_effect=RuntimeError("secret recipient data"),
        ):
            self.assertTrue(process_next_password_reset_mail())
        queued = PasswordResetMail.objects.get(user=self.user)
        self.assertEqual(queued.status, "pending")
        self.assertEqual(queued.last_error_kind, "RuntimeError")
        self.assertNotIn(self.user.email, queued.last_error_kind)
        self.assertGreater(queued.next_attempt_at, queued.last_attempt_at)

    def test_reset_link_expires_after_24_hours(self):
        client = Client(enforce_csrf_checks=True)
        issued_at = datetime(2026, 8, 28, 12, 0, 0)
        self._post(
            client,
            "/api/account/password-reset-requests",
            {"email": self.user.email},
        )
        with patch.object(default_token_generator, "_now", return_value=issued_at):
            process_next_password_reset_mail()
        reset_link = next(
            word for word in mail.outbox[0].body.split() if "/reset-password#" in word
        )
        reset_values = parse_qs(urlsplit(reset_link).fragment)
        payload = {
            "uid": reset_values["uid"][0],
            "token": reset_values["token"][0],
            "newPassword": self.new_password,
        }

        with patch.object(
            default_token_generator,
            "_now",
            return_value=issued_at + timedelta(hours=24, seconds=1),
        ):
            response = self._post(
                client,
                "/api/account/password-resets",
                payload,
            )
        self.assertEqual(response.status_code, 400)
        self.assertEqual(
            response.json(), {"error": "account_password_reset_invalid"}
        )
