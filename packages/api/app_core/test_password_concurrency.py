"""Password mutations serialize on authoritative user state, not request copies."""

from concurrent.futures import ThreadPoolExecutor
from threading import Event
from time import monotonic
from types import SimpleNamespace
from unittest.mock import patch

from django.contrib.auth import get_user_model
from django.contrib.auth.tokens import default_token_generator
from django.db import connection, connections, close_old_connections
from django.test import TransactionTestCase
from django.utils.encoding import force_bytes
from django.utils.http import urlsafe_base64_encode

from app_core.http import auth
from app_core.password_reset import reset_password


class PasswordConcurrencyTests(TransactionTestCase):
    serialized_rollback = True

    old = "Initial-Audit-Password!2026"
    first = "First-Replacement-Password!2026"
    second = "Second-Replacement-Password!2026"

    def setUp(self):
        self.user = get_user_model().objects.create_user(username="password-race", password=self.old)
        self.uid = urlsafe_base64_encode(force_bytes(self.user.pk))
        self.token = default_token_generator.make_token(self.user)

    def race(self, first_kind, second_kind):
        entered, release, second_started = Event(), Event(), Event()
        second_pid = []
        # Deliberately retain stale authenticated request objects in both lanes.
        users = [get_user_model().objects.get(pk=self.user.pk) for _ in range(2)]
        def action(kind, lane):
            close_old_connections()
            try:
                if lane == 1:
                    with connection.cursor() as cursor:
                        cursor.execute("SELECT pg_backend_pid()")
                        second_pid.append(cursor.fetchone()[0])
                    second_started.set()
                password = self.first if lane == 0 else self.second
                if kind == "reset":
                    return reset_password(self.uid, self.token, password)
                response = auth.change_password(SimpleNamespace(user=users[lane]), SimpleNamespace(current_password=self.old, new_password=password))
                return None if isinstance(response, dict) else response.value["error"]
            finally:
                connections.close_all()
        if first_kind == "reset":
            original = default_token_generator.check_token
            target, name = default_token_generator, "check_token"
        else:
            original = auth.validate_password
            target, name = auth, "validate_password"
        def gate(*args, **kwargs):
            value = original(*args, **kwargs)
            if not entered.is_set():
                entered.set()
                if not release.wait(10):
                    raise TimeoutError("password race release timed out")
            return value
        with patch.object(target, name, side_effect=gate), patch.object(auth, "update_session_auth_hash"), ThreadPoolExecutor(max_workers=2) as pool:
            first = pool.submit(action, first_kind, 0)
            try:
                self.assertTrue(entered.wait(10))
                second = pool.submit(action, second_kind, 1)
                self.assertTrue(second_started.wait(10))
                # Old code completes the second mutation. Fixed code waits on
                # the first transaction's row lock. Observe either, without
                # placing a barrier inside the protected critical section.
                deadline = monotonic() + 10
                while not second.done():
                    with connection.cursor() as cursor:
                        cursor.execute("SELECT cardinality(pg_blocking_pids(%s))", [second_pid[0]])
                        if cursor.fetchone()[0]:
                            break
                    self.assertLess(monotonic(), deadline)
                    release.wait(0.01)
            finally:
                release.set()
            results = [first.result(timeout=10), second.result(timeout=10)]
        expected_error = "account_password_reset_invalid" if second_kind == "reset" else "account_current_password_invalid"
        self.assertEqual(results, [None, expected_error])
        self.user.refresh_from_db()
        self.assertTrue(self.user.check_password(self.first))
        self.assertFalse(self.user.check_password(self.second))

    def test_same_reset_token_has_one_concurrent_winner(self):
        self.race("reset", "reset")

    def test_reset_prevents_stale_authenticated_password_change(self):
        self.race("reset", "change")

    def test_password_change_invalidates_inflight_reset(self):
        self.race("change", "reset")

    def test_stale_authenticated_changes_have_one_winner(self):
        self.race("change", "change")
