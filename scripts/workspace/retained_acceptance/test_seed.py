from contextlib import nullcontext
import io
import json
import os
from pathlib import Path
import runpy
import sys
import tempfile
from types import ModuleType, SimpleNamespace
import unittest
from unittest.mock import patch


class InputManager:
    def __init__(self, name):
        self.name=name
        self.created=[]

    def exists(self):
        return False

    def create(self, **fields):
        value=SimpleNamespace(id=self.name+'-'+str(len(self.created)+1), **fields)
        self.created.append(value)
        return value

    create_user=create


class SeedFixtureTests(unittest.TestCase):
    def test_seed_uses_media_root_and_refuses_to_replace_private_output(self):
        with tempfile.TemporaryDirectory() as directory:
            media=Path(directory)/'private-storage'
            settings=SimpleNamespace(DATABASES={'default':{'NAME':'retained_acceptance'}},
                                     MEDIA_ROOT=str(media))
            names=('Agent','ModelConfig','ModelProvider','ProviderCredential','Workspace','WorkspaceMembership')
            managers={name:InputManager(name) for name in (*names,'User')}
            models=ModuleType('app_core.models')
            for name in names:
                setattr(models,name,SimpleNamespace(objects=managers[name]))
            conf=ModuleType('django.conf')
            conf.settings=settings
            auth=ModuleType('django.contrib.auth')
            auth.get_user_model=lambda:SimpleNamespace(objects=managers['User'])
            db=ModuleType('django.db')
            db.connection=SimpleNamespace()
            db.transaction=SimpleNamespace(atomic=nullcontext)
            credentials=ModuleType('app_core.credentials')
            credentials.encrypt_credential_secret=lambda value:'synthetic-encrypted-placeholder'
            modules={'django':ModuleType('django'),'django.conf':conf,
                'django.contrib':ModuleType('django.contrib'),'django.contrib.auth':auth,
                'django.db':db,'app_core':ModuleType('app_core'),
                'app_core.models':models,'app_core.credentials':credentials}
            script=Path(__file__).with_name('seed.py')
            stdout=io.StringIO()
            with patch.dict(sys.modules,modules), \
                    patch.dict(os.environ,{'AGENT_RUN_EXECUTION_MODE':'retained'}), \
                    patch('sys.stdout',stdout):
                runpy.run_path(str(script),run_name='__main__')
                output=media/'acceptance-seed.json'
                self.assertTrue(output.is_file(),'private seed must use configured MEDIA_ROOT')
                original=output.read_bytes()
                seed=json.loads(original)
                self.assertTrue(set(seed)=={'email','password','workspaceId','agentId','modelConfigRef'})
                self.assertEqual(seed['workspaceId'],managers['Workspace'].created[0].id)
                self.assertEqual(seed['agentId'],managers['Agent'].created[0].id)
                self.assertEqual(seed['modelConfigRef'],managers['ModelConfig'].created[0].id)
                user=managers['User'].created[0]
                self.assertTrue(seed['email']==user.email and seed['email'].endswith('@fixture.invalid'))
                self.assertTrue(seed['password']==user.password and len(seed['password'])>=40)
                self.assertTrue(seed['password'] not in stdout.getvalue(),
                    'credential values must not appear in fixture output')
                if os.name!='nt':
                    self.assertEqual(output.stat().st_mode & 0o777,0o600)
                counts={name:len(manager.created) for name,manager in managers.items()}
                with self.assertRaisesRegex(AssertionError,'refusing to replace fixture credentials'):
                    runpy.run_path(str(script),run_name='__main__')
                self.assertTrue(output.read_bytes()==original,'existing private output must remain unchanged')
                self.assertTrue({name:len(manager.created) for name,manager in managers.items()}==counts,
                    'output conflict must be rejected before creating more business inputs')


if __name__=='__main__':
    unittest.main()
