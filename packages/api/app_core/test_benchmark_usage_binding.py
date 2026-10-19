"""Real PostgreSQL ownership checks for the controller's numeric usage reader."""
import copy
import importlib.util
import json
from pathlib import Path
from unittest import skipUnless

from django.db import connection
from django.test import TestCase

from .environment_profile import environment_profile_digest
from .runtime_contract import validate_agent_run_authorization_payload


ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location('benchmark_usage_metrics', ROOT / 'scripts/harbor/workspace_metrics.py')
METRICS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(METRICS)


@skipUnless(connection.vendor == 'postgresql', 'requires real PostgreSQL JSON and scope semantics')
class BenchmarkUsageBindingTests(TestCase):
    def setUp(self):
        # Temp relations shadow only this connection's synthetic scope. The
        # production query runs unchanged against real PostgreSQL JSONB values.
        with connection.cursor() as cursor:
            cursor.execute('CREATE TEMP TABLE app_core_sessionenvironment (id text PRIMARY KEY, session_id text, '
                'environment_profile jsonb, environment_profile_digest text, network text) ON COMMIT DROP')
            cursor.execute('CREATE TEMP TABLE app_core_agentrun (id text PRIMARY KEY, session_id text, environment_id text) ON COMMIT DROP')
            cursor.execute('CREATE TEMP TABLE app_core_agentrunauthorization (agent_run_id text PRIMARY KEY, payload jsonb) ON COMMIT DROP')
            cursor.execute('SELECT current_database(), current_user')
            database, user = cursor.fetchone()
        self.scope = {'expected_db': database, 'expected_user': user, 'session_id': 'sess_1', 'run_id': 'agent_run_1'}

    def payload(self, version):
        name = 'v1/valid.json' if version == 0 else 'v1/custom.json'
        value = json.loads((ROOT / 'tests/workspace/fixtures/agent_run_authorization' / name).read_text(encoding='utf-8'))
        validate_agent_run_authorization_payload(value)
        return value

    def bind(self, payload, *, profile=None, network=None, environment_session='sess_1'):
        profile = profile if profile is not None else payload.get('environmentProfile', self.payload(1)['environmentProfile'])
        with connection.cursor() as cursor:
            cursor.execute('DELETE FROM app_core_agentrunauthorization')
            cursor.execute('DELETE FROM app_core_agentrun')
            cursor.execute('DELETE FROM app_core_sessionenvironment')
            cursor.execute('INSERT INTO app_core_sessionenvironment VALUES (%s,%s,%s::jsonb,%s,%s)',
                ['environment', environment_session, json.dumps(profile), environment_profile_digest(profile), network or profile['network']])
            cursor.execute('INSERT INTO app_core_agentrun VALUES (%s,%s,%s)', ['agent_run_1', 'sess_1', 'environment'])
            cursor.execute('INSERT INTO app_core_agentrunauthorization VALUES (%s,%s::jsonb)', ['agent_run_1', json.dumps(payload)])

    def owned(self, **scope):
        with connection.cursor() as cursor:
            cursor.execute(METRICS.OwnedUsageReader._scope_sql(lambda name: '%(' + name + ')s'), self.scope | scope)
            return cursor.fetchone()[0]

    def test_valid_custom_profile_scope_is_owned(self):
        self.bind(self.payload(1))
        self.assertIs(self.owned(), True)

    def test_scope_is_owned_for_both_signed_network_policies(self):
        for network in ('disabled', 'publicInternet'):
            payload = self.payload(1)
            payload['environmentProfile']['network'] = network
            payload['environmentProfileDigest'] = environment_profile_digest(payload['environmentProfile'])
            validate_agent_run_authorization_payload(payload)
            self.bind(payload)
            with self.subTest(network=network):
                self.assertIs(self.owned(), True)

    def test_profiles_cannot_read_foreign_session_run_database_or_role(self):
        for version in (0, 1):
            self.bind(self.payload(version))
            for scope in ({'session_id': 'foreign-session'}, {'run_id': 'foreign-run'},
                    {'expected_db': 'foreign-database'}, {'expected_user': 'foreign-role'}):
                with self.subTest(version=version, scope=scope):
                    self.assertIs(self.owned(**scope), False)
            self.bind(self.payload(version), environment_session='foreign-session')
            self.assertIs(self.owned(), False)

    def test_profile_digest_and_network_mismatch_never_prove_ownership(self):
        for version in (0, 1):
            payload = self.payload(version)
            changed = copy.deepcopy(payload['environmentProfile'])
            changed['workspaceRoot'] = '/different'
            self.bind(payload, profile=changed)
            self.assertIs(self.owned(), False)
            self.bind(payload, network='publicInternet' if payload['environmentProfile']['network'] == 'disabled' else 'disabled')
            with self.subTest(version=version, mismatch='environment network'):
                self.assertIs(self.owned(), False)

    def test_default_profile_is_the_same_owned_scope_and_unsupported_schema_is_rejected(self):
        value = self.payload(0)
        self.bind(value)
        self.assertIs(self.owned(), True)
        self.bind(value | {'schema': 'unsupported'})
        self.assertIs(self.owned(), False)
