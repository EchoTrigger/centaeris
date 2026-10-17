"""Synthetic tests for real-provider business seeding; no database or model requests."""
from contextlib import nullcontext
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tempfile
from types import ModuleType, SimpleNamespace
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).with_name('benchmark_seed.py')
sys.path.insert(0, str(SCRIPT.resolve().parents[2] / 'harbor'))
MODEL = {'apiBase': 'https://models.example.org/v1', 'credentialEnv': 'EXAMPLE_MODEL_KEY',
         'id': 'example/reasoner', 'thinking': 'low', 'contextTokens': 32768, 'outputTokens': 4096}


class Manager:
    def __init__(self, name):
        self.name, self.created = name, []
        self.occupied = False

    def exists(self):
        return self.occupied

    def create(self, **fields):
        value = SimpleNamespace(id=self.name + '-1', **fields)
        self.created.append(value)
        return value

    create_user = create


class Cursor:
    def __init__(self, database, user, migrated=True):
        self.database, self.user, self.migrated = database, user, migrated
        self.query = ''

    def __enter__(self):
        return self

    def __exit__(self, *args):
        pass

    def execute(self, query, parameters=None):
        self.query = query

    def fetchone(self):
        return (self.database, self.user) if 'current_database' in self.query else (self.migrated,)


class BenchmarkSeedTests(unittest.TestCase):
    def setup_seed(self, root, *, actual_database=None, migrated=True):
        identity = {'schema': 'centaeris.workspaceBenchmarkFixture.v1', 'fixtureId': 'tbworkspace' + 'a'*10,
            'databaseName': 'tbworkspace' + 'a'*10, 'databaseUser': 'tbworkspace' + 'a'*10,
            'sourceRevision': 'b'*40, 'sourceTree': 'c'*40,
            'dockerHostId': 'daemon-owned', 'dockerSocket': '/var/run/docker.sock'}
        identity_path = root / 'fixture.json'
        identity_path.write_text(json.dumps(identity))
        media = root / 'private'
        settings = SimpleNamespace(DATABASES={'default': {'NAME': identity['databaseName'], 'USER': identity['databaseUser']}}, MEDIA_ROOT=str(media))
        names = ('Agent','ModelConfig','ModelProvider','ProviderCredential','Workspace','WorkspaceMembership','Session','AgentRun','User')
        managers = {name: Manager(name) for name in names}
        models = ModuleType('app_core.models')
        for name in names[:-1]:
            setattr(models, name, SimpleNamespace(objects=managers[name]))
        conf = ModuleType('django.conf'); conf.settings = settings
        auth = ModuleType('django.contrib.auth'); auth.get_user_model = lambda: SimpleNamespace(objects=managers['User'])
        db = ModuleType('django.db')
        db.connection = SimpleNamespace(vendor='postgresql', cursor=lambda: Cursor(
            actual_database or identity['databaseName'], identity['databaseUser'], migrated))
        db.transaction = SimpleNamespace(atomic=nullcontext)
        encrypted = []
        credentials = ModuleType('app_core.credentials')
        credentials.encrypt_credential_secret = lambda value: encrypted.append(value) or 'encrypted-synthetic-ciphertext'
        modules = {'django': ModuleType('django'), 'django.conf': conf,
            'django.contrib': ModuleType('django.contrib'), 'django.contrib.auth': auth,
            'django.db': db, 'app_core': ModuleType('app_core'), 'app_core.models': models,
            'app_core.credentials': credentials}
        spec = importlib.util.spec_from_file_location('benchmark_seed', SCRIPT)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return identity_path, settings, managers, modules, encrypted, module

    def test_real_model_only_business_inputs_and_no_plaintext_key_persistence(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            identity, settings, managers, modules, encrypted, module = self.setup_seed(root)
            secret = 'synthetic-provider-secret-never-persisted'
            stream = io.StringIO(json.dumps({'schema': 'centaeris.workspaceBenchmarkSeedInput.v1', 'credential': secret, 'model': MODEL}))
            stdout = io.StringIO()
            with patch.dict(sys.modules, modules), patch.dict(os.environ,
                    {'CENTAERIS_BENCHMARK_FIXTURE_ID': 'tbworkspace'+'a'*10, 'AGENT_RUN_EXECUTION_MODE': 'retained'}), patch('sys.stdout', stdout):
                module.seed(identity, stream)
                output = Path(settings.MEDIA_ROOT) / 'benchmark-seed.json'
                saved = json.loads(output.read_text())
                self.assertEqual(encrypted, [secret])
                self.assertNotIn(secret, output.read_text())
                self.assertNotIn(secret, stdout.getvalue())
                self.assertNotIn(saved['password'], stdout.getvalue())
                self.assertEqual(managers['ModelProvider'].created[0].apiBase, MODEL['apiBase'])
                model = managers['ModelConfig'].created[0]
                self.assertEqual((model.modelName, model.contextTokens, model.maxOutputTokens,
                                  model.thinkingMode, model.thinkingModes),
                                 (MODEL['id'], MODEL['contextTokens'], MODEL['outputTokens'], MODEL['thinking'], [MODEL['thinking']]))
                self.assertEqual(saved['model'], MODEL)
                from workspace_plan import canonical_digest
                self.assertEqual(saved['modelSha256'], canonical_digest(MODEL))
                self.assertEqual(len(managers['Session'].created), 0)
                self.assertEqual(len(managers['AgentRun'].created), 0)
                self.assertEqual(saved['workspaceId'], managers['Workspace'].created[0].id)
                self.assertTrue(saved['email'].endswith('@fixture.invalid'))
                counts = {name: len(manager.created) for name, manager in managers.items()}
                with self.assertRaisesRegex(ValueError, 'existing_private_seed'):
                    module.seed(identity, io.StringIO(json.dumps({'schema': 'centaeris.workspaceBenchmarkSeedInput.v1', 'credential': secret, 'model': MODEL})))
                self.assertEqual(counts, {name: len(manager.created) for name, manager in managers.items()})
                if os.name != 'nt':
                    self.assertEqual(output.stat().st_mode & 0o777, 0o600)

    def test_different_model_without_thinking_is_seeded_from_same_descriptor(self):
        from workspace_plan import canonical_digest
        model = dict(MODEL, id='other/plain', apiBase='https://other.example.net/api/v1',
                     thinking=None, contextTokens=16384, outputTokens=2048)
        with tempfile.TemporaryDirectory() as tmp:
            identity, settings, managers, modules, encrypted, module = self.setup_seed(Path(tmp))
            stream = io.StringIO(json.dumps({'schema': module.INPUT_SCHEMA, 'credential': 'synthetic', 'model': model}))
            with patch.dict(sys.modules, modules), patch.dict(os.environ,
                    {'CENTAERIS_BENCHMARK_FIXTURE_ID': 'tbworkspace'+'a'*10, 'AGENT_RUN_EXECUTION_MODE': 'retained'}), patch('builtins.print'):
                module.seed(identity, stream)
            configured = managers['ModelConfig'].created[0]
            self.assertEqual(configured.modelName, model['id'])
            self.assertEqual(configured.thinkingMode, '')
            self.assertEqual(configured.thinkingModes, [])
            saved = json.loads((Path(settings.MEDIA_ROOT) / 'benchmark-seed.json').read_text())
            self.assertEqual(saved['modelSha256'], canonical_digest(model))

    def test_wrong_database_migration_or_occupied_database_cannot_seed(self):
        for bad in ('database', 'migration', 'occupied', 'fixture'):
            with self.subTest(bad=bad), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                identity, settings, managers, modules, encrypted, module = self.setup_seed(root,
                    actual_database='production' if bad == 'database' else None, migrated=bad != 'migration')
                if bad == 'occupied':
                    managers['Workspace'].occupied = True
                with patch.dict(sys.modules, modules), patch.dict(os.environ,
                        {'CENTAERIS_BENCHMARK_FIXTURE_ID': 'other' if bad == 'fixture' else 'tbworkspace'+'a'*10,
                         'AGENT_RUN_EXECUTION_MODE': 'retained'}):
                    with self.assertRaises(ValueError):
                        module.seed(identity, io.StringIO(json.dumps({'schema': 'centaeris.workspaceBenchmarkSeedInput.v1', 'credential': 'synthetic-key', 'model': MODEL})))
                self.assertFalse(encrypted)
                self.assertFalse(any(manager.created for manager in managers.values()))
                self.assertFalse((Path(settings.MEDIA_ROOT) / 'benchmark-seed.json').exists())

    def test_model_validation_precedes_business_input_creation(self):
        for changes in ({'apiBase': 'https://secret@example.org/v1'},
                        {'outputTokens': MODEL['contextTokens']}, {'unknown': True}):
            with self.subTest(changes=changes), tempfile.TemporaryDirectory() as tmp:
                identity, settings, managers, modules, encrypted, module = self.setup_seed(Path(tmp))
                stream = io.StringIO(json.dumps({'schema': module.INPUT_SCHEMA, 'credential': 'synthetic',
                                                'model': dict(MODEL, **changes)}))
                with patch.dict(sys.modules, modules), patch.dict(os.environ,
                        {'CENTAERIS_BENCHMARK_FIXTURE_ID': 'tbworkspace'+'a'*10, 'AGENT_RUN_EXECUTION_MODE': 'retained'}):
                    with self.assertRaises(ValueError):
                        module.seed(identity, stream)
                self.assertFalse(encrypted)
                self.assertFalse(any(manager.created for manager in managers.values()))

    def test_input_rejects_missing_unknown_duplicate_or_oversize_credentials(self):
        for wire in ('{}', '{"schema":"old","credential":"fake"}',
                     '{"schema":"centaeris.workspaceBenchmarkSeedInput.v1","credential":"fake","extra":1}',
                     '{"schema":"centaeris.workspaceBenchmarkSeedInput.v1","credential":"a","credential":"b"}',
                     'x' * 16385):
            with self.subTest(size=len(wire)), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                identity, settings, managers, modules, encrypted, module = self.setup_seed(root)
                with patch.dict(sys.modules, modules), patch.dict(os.environ,
                        {'CENTAERIS_BENCHMARK_FIXTURE_ID': 'tbworkspace'+'a'*10, 'AGENT_RUN_EXECUTION_MODE': 'retained'}):
                    with self.assertRaises(ValueError):
                        module.seed(identity, io.StringIO(wire))
                self.assertFalse(encrypted)
                self.assertFalse(any(manager.created for manager in managers.values()))


if __name__ == '__main__':
    unittest.main()
