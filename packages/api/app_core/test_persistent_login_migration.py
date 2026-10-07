"""Existing browser credentials have one authority after the forward upgrade."""
from datetime import timedelta
from importlib import import_module
from unittest.mock import patch

from django.contrib.auth import get_user_model
from django.db import connection
from django.db.migrations.executor import MigrationExecutor
from django.test import Client, TransactionTestCase, override_settings
from django.utils import timezone


class PersistentBrowserLoginMigrationTests(TransactionTestCase):
    serialized_rollback = True
    migrate_from = [("app_core", "0002_persistent_app_delegations"), ("sessions", "0001_initial")]
    migrate_to = [("app_core", "0003_persistent_browser_login")]

    def test_old_signed_bytes_keys_and_deadlines_move_without_reviving_expired_credentials(self):
        executor = MigrationExecutor(connection)
        executor.migrate(self.migrate_from)
        old = executor.loader.project_state(self.migrate_from).apps
        try:
            legacy = old.get_model("sessions", "Session")
            now = timezone.now()
            rows = [
                {"session_key": "valid-browser", "session_data": "signed-raw-credential-unchanged",
                 "expire_date": now + timedelta(hours=1)},
                {"session_key": "expired-browser", "session_data": "expired-signed-bytes-unchanged",
                 "expire_date": now - timedelta(hours=1)},
            ]
            for row in rows:
                legacy.objects.create(**row)
            executor = MigrationExecutor(connection)
            executor.migrate(self.migrate_to)
            current = executor.loader.project_state(self.migrate_to).apps
            moved = current.get_model("app_core", "BrowserLoginSession")
            self.assertEqual(list(moved.objects.order_by("session_key").values()),
                             sorted(rows, key=lambda row: row["session_key"]))
            self.assertFalse(current.get_model("sessions", "Session").objects.exists())
            store = import_module("app_core.persistent_sessions").SessionStore
            self.assertIsNotNone(store("valid-browser")._get_session_from_db())
            self.assertIsNone(store("expired-browser")._get_session_from_db())
        finally:
            latest = MigrationExecutor(connection)
            latest.migrate(latest.loader.graph.leaf_nodes("app_core"))

    def test_downgrade_with_credentials_is_refused_before_their_table_can_be_dropped(self):
        executor = MigrationExecutor(connection)
        executor.migrate(self.migrate_to)
        current = executor.loader.project_state(self.migrate_to).apps
        model = current.get_model("app_core", "BrowserLoginSession")
        model.objects.create(session_key="persistent-browser", session_data="unchanged", expire_date=None)
        with self.assertRaisesRegex(RuntimeError, "browser credentials"):
            MigrationExecutor(connection).migrate(self.migrate_from)
        self.assertEqual(model.objects.get(session_key="persistent-browser").session_data, "unchanged")

    def test_migrated_real_login_adopts_policy_and_logout_has_no_legacy_fallback(self):
        executor = MigrationExecutor(connection)
        executor.migrate(self.migrate_from)
        try:
            user = get_user_model().objects.create_user(username="old-login-upgrade")
            now = timezone.now()
            browser = Client()
            expired_browser = Client()
            with override_settings(SESSION_ENGINE="django.contrib.sessions.backends.db", SESSION_COOKIE_AGE=8 * 60 * 60):
                with patch("django.utils.timezone.now", return_value=now):
                    browser.force_login(user)
                key = browser.session.session_key
                with patch("django.utils.timezone.now", return_value=now - timedelta(hours=9)):
                    expired_browser.force_login(user)
            old = executor.loader.project_state(self.migrate_from).apps.get_model("sessions", "Session")
            original = old.objects.get(pk=key)
            original_data, original_expiry = original.session_data, original.expire_date
            executor = MigrationExecutor(connection)
            executor.migrate(self.migrate_to)
            current = executor.loader.project_state(self.migrate_to).apps
            login = current.get_model("app_core", "BrowserLoginSession")
            migrated = login.objects.get(pk=key)
            self.assertEqual((migrated.session_data, migrated.expire_date), (original_data, original_expiry))
            with patch("django.utils.timezone.now", return_value=now + timedelta(hours=7)):
                self.assertEqual(browser.get("/api/me").status_code, 200)
                self.assertEqual(expired_browser.get("/api/me").status_code, 401)
                self.assertEqual(browser.session.session_key, key)
                self.assertIsNone(login.objects.get(pk=key).expire_date)
                csrf = browser.get("/api/csrf").json()["csrfToken"]
                self.assertEqual(browser.post("/api/logout", HTTP_X_CSRFTOKEN=csrf).status_code, 200)
            self.assertFalse(login.objects.filter(pk=key).exists())
            self.assertFalse(old.objects.filter(pk=key).exists())
            browser.cookies["sessionid"] = key
            self.assertEqual(browser.get("/api/me").status_code, 401)
        finally:
            latest = MigrationExecutor(connection)
            latest.migrate(latest.loader.graph.leaf_nodes("app_core"))
