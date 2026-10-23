"""A durable provider retirement rejects new grants under the Environment lock."""
from contextlib import nullcontext
from types import SimpleNamespace
from unittest.mock import MagicMock, patch

from django.test import SimpleTestCase

from .environment_profile import environment_profile_digest, freeze_run_environment_profile
from .test_provider_cleanup_receipt import provider_profile


class ProviderEnvironmentAdmissionTests(SimpleTestCase):
    def freeze(self, *, retiring, lifetime='retained'):
        profile = provider_profile() | {'lifetime': lifetime}
        environment = SimpleNamespace(pk='environment-1', session_id='session-1',
            environment_profile=profile, environment_profile_digest=environment_profile_digest(profile))
        session = SimpleNamespace(pk='session-1', current_environment_id=environment.pk)
        run = SimpleNamespace(pk='run-2', session_id=session.pk, environment_id=environment.pk, session=session)
        cursor = MagicMock()
        cursor.fetchone.return_value = (retiring,)
        manager = MagicMock()
        manager.__enter__.return_value = cursor
        with patch('app_core.environment_profile.transaction.atomic', return_value=nullcontext()), \
                patch('app_core.models.Session.objects.select_for_update') as sessions, \
                patch('app_core.models.SessionEnvironment.objects.select_for_update') as environments, \
                patch('app_core.environment_profile.connection', create=True) as database:
            database.cursor.return_value = manager
            sessions.return_value.get.return_value = session
            environments.return_value.get.return_value = environment
            actual = freeze_run_environment_profile(run)
        return actual, environment, cursor

    def test_retirement_intent_rejects_profile_resolution_before_a_new_grant(self):
        with self.assertRaisesMessage(ValueError, 'environment_retiring'):
            self.freeze(retiring=True)

    def test_same_locked_environment_without_retirement_remains_authoritative(self):
        actual, expected, _ = self.freeze(retiring=False)
        self.assertIs(actual, expected)
        self.assertEqual(actual.environment_profile['lifetime'], 'retained')

    def test_per_run_contract_does_not_depend_on_retained_provider_storage(self):
        actual, _, cursor = self.freeze(retiring=True, lifetime='perRun')
        self.assertEqual(actual.environment_profile['lifetime'], 'perRun')
        cursor.execute.assert_not_called()
