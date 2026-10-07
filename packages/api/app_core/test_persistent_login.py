"""Active browser sessions renew without changing explicit revocation behavior."""
from datetime import datetime, timedelta, timezone
from unittest.mock import patch

from asgiref.sync import async_to_sync
from django.contrib.auth import SESSION_KEY, get_user_model
from django.contrib.sessions.models import Session as LegacySession
from django.conf import settings
from django.db import connection
from django.test import Client, TestCase
from django.test.utils import CaptureQueriesContext, override_settings


class PersistentLoginTests(TestCase):
    started_at = datetime(2026, 1, 1, tzinfo=timezone.utc)

    def setUp(self):
        self.user = get_user_model().objects.create_user(username="persistent-login@example.test")
        self.browser = Client()
        with patch("django.utils.timezone.now", return_value=self.started_at):
            self.browser.force_login(self.user)
        self.session_key = self.browser.session.session_key

    def request_at(self, elapsed):
        with patch("django.utils.timezone.now", return_value=self.started_at + elapsed):
            return self.browser.get("/api/me")

    def login_model(self):
        from importlib import import_module
        return import_module(settings.SESSION_ENGINE).SessionStore.get_model_class()

    def test_authenticated_requests_keep_unbounded_server_and_persistent_browser_credential(self):
        response = self.request_at(timedelta(hours=7))
        self.assertEqual(response.status_code, 200)
        self.assertIsNone(self.login_model().objects.get(session_key=self.session_key).expire_date)
        cookie = response.cookies["sessionid"]
        self.assertTrue(cookie["expires"])
        self.assertGreater(int(cookie["max-age"]), 8 * 60 * 60)
        self.assertTrue(cookie["httponly"])
        self.assertEqual(cookie["samesite"], "Lax")

    def test_continued_use_crosses_original_eight_hour_boundary(self):
        self.assertEqual(self.request_at(timedelta(hours=7)).status_code, 200)
        self.assertEqual(self.request_at(timedelta(hours=9)).status_code, 200)
        self.assertEqual(self.request_at(timedelta(days=29)).status_code, 200)
        self.assertEqual(self.request_at(timedelta(days=40)).status_code, 200)

    def test_server_credential_remains_valid_after_years_without_requests(self):
        response = self.request_at(timedelta(days=3650))
        self.assertEqual(response.status_code, 200)
        self.assertEqual(self.browser.session.session_key, self.session_key)

    def test_unchanged_authenticated_get_renews_cookie_without_writing_server_records(self):
        with CaptureQueriesContext(connection) as queries:
            response = self.request_at(timedelta(days=40))
        self.assertEqual(response.status_code, 200)
        self.assertTrue(response.cookies["sessionid"]["expires"])
        self.assertFalse(any(query["sql"].lstrip().upper().startswith(("INSERT", "UPDATE", "DELETE"))
                             for query in queries))

    def test_revoking_the_stored_credential_rejects_the_same_browser_cookie(self):
        self.login_model().objects.filter(session_key=self.session_key).delete()
        self.assertEqual(self.request_at(timedelta(hours=1)).status_code, 401)

    def test_explicit_logout_invalidates_the_persistent_session(self):
        self.assertEqual(self.request_at(timedelta(hours=7)).status_code, 200)
        with patch("django.utils.timezone.now", return_value=self.started_at + timedelta(hours=7)):
            csrf = self.browser.get("/api/csrf").json()["csrfToken"]
            response = self.browser.post("/api/logout", HTTP_X_CSRFTOKEN=csrf)
        self.assertEqual(response.status_code, 200)
        self.assertEqual(self.request_at(timedelta(hours=9)).status_code, 401)

    def test_disabled_account_cannot_renew_the_session(self):
        self.user.is_active = False
        self.user.save(update_fields=["is_active"])
        self.assertEqual(self.request_at(timedelta(hours=7)).status_code, 401)

    def test_password_change_invalidates_the_original_login_credential(self):
        self.user.set_password("new-credential-for-test")
        self.user.save(update_fields=["password"])
        self.assertEqual(self.request_at(timedelta(hours=1)).status_code, 401)

    def legacy_browser(self):
        browser = Client()
        with override_settings(SESSION_COOKIE_AGE=8 * 60 * 60, SESSION_EXPIRE_AT_BROWSER_CLOSE=True,
                               SESSION_SAVE_EVERY_REQUEST=False, SESSION_ENGINE="django.contrib.sessions.backends.db"):
            with patch("django.utils.timezone.now", return_value=self.started_at):
                browser.force_login(self.user)
        if self.login_model() is not LegacySession:
            # The migration transfers old signed records without changing their key/data/deadline.
            legacy = LegacySession.objects.get(session_key=browser.session.session_key)
            self.login_model().objects.create(session_key=legacy.session_key,
                session_data=legacy.session_data, expire_date=legacy.expire_date)
            legacy.delete()
        return browser

    def test_existing_unexpired_session_adopts_persistent_policy_on_next_request(self):
        browser = self.legacy_browser()
        session_key = browser.session.session_key
        with patch("django.utils.timezone.now", return_value=self.started_at + timedelta(hours=7)):
            response = browser.get("/api/me")
        self.assertEqual(response.status_code, 200)
        self.assertEqual(browser.session.session_key, session_key)
        self.assertIsNone(self.login_model().objects.get(session_key=session_key).expire_date)
        self.assertTrue(response.cookies["sessionid"]["expires"])

    def test_existing_expired_session_is_not_revived_by_the_new_policy(self):
        browser = self.legacy_browser()
        with patch("django.utils.timezone.now", return_value=self.started_at + timedelta(hours=9)):
            response = browser.get("/api/me")
        self.assertEqual(response.status_code, 401)


class PersistentSessionStoreTests(TestCase):
    def test_saving_a_loaded_credential_after_revocation_does_not_recreate_it(self):
        from django.contrib.sessions.backends.base import UpdateError
        from .persistent_sessions import SessionStore
        original = SessionStore()
        original[SESSION_KEY] = "42"
        original.save()
        loaded = SessionStore(original.session_key)
        self.assertEqual(loaded[SESSION_KEY], "42")
        original.delete()
        with self.assertRaises(UpdateError):
            loaded.save()
        self.assertFalse(original.exists(original.session_key))

    def test_nested_session_data_changes_are_saved_even_when_modified_flag_is_false(self):
        from .persistent_sessions import SessionStore
        original = SessionStore()
        original[SESSION_KEY] = "42"
        original["nested"] = {"value": "before"}
        original.save()
        loaded = SessionStore(original.session_key)
        loaded["nested"]["value"] = "after"
        self.assertFalse(loaded.modified)
        loaded.save()
        self.assertEqual(SessionStore(original.session_key).load()["nested"]["value"], "after")

    def test_explicit_deadline_and_anonymous_sessions_keep_finite_expiry(self):
        from .persistent_sessions import SessionStore
        authenticated = SessionStore()
        authenticated[SESSION_KEY] = "42"
        authenticated.set_expiry(60)
        authenticated.save()
        anonymous = SessionStore()
        anonymous["anonymous-data"] = "value"
        anonymous.save()
        model = SessionStore.get_model_class()
        self.assertIsNotNone(model.objects.get(session_key=authenticated.session_key).expire_date)
        self.assertIsNotNone(model.objects.get(session_key=anonymous.session_key).expire_date)
        after_deadline = datetime.now(timezone.utc) + timedelta(minutes=2)
        with patch("django.utils.timezone.now", return_value=after_deadline):
            self.assertEqual(SessionStore(authenticated.session_key).load(), {})

    def test_standard_expired_cleanup_preserves_persistent_credentials(self):
        from .persistent_sessions import SessionStore
        model = SessionStore.get_model_class()
        model.objects.create(session_key="persistent", session_data="signed", expire_date=None)
        model.objects.create(session_key="expired", session_data="signed",
                             expire_date=datetime.now(timezone.utc) - timedelta(days=1))
        SessionStore.clear_expired()
        self.assertTrue(model.objects.filter(session_key="persistent").exists())
        self.assertFalse(model.objects.filter(session_key="expired").exists())

    def test_async_session_backend_uses_the_same_permanent_and_revocable_authority(self):
        from .persistent_sessions import SessionStore

        async def exercise():
            store = SessionStore()
            await store.aset(SESSION_KEY, "42")
            await store.asave()
            session_key = store.session_key
            self.assertIsNone((await SessionStore.get_model_class().objects.aget(session_key=session_key)).expire_date)
            self.assertEqual(await SessionStore(session_key).aload(), {SESSION_KEY: "42"})
            await store.adelete(session_key)
            self.assertEqual(await SessionStore(session_key).aload(), {})

        async_to_sync(exercise)()
