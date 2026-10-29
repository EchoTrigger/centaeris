import contextlib
import json
import os
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import Mock,patch


class SeedModeTests(unittest.TestCase):
 def execute(self,folder,mode,shared=True,recovery=False):
  managers={name:Mock() for name in ('Agent','ModelConfig','ModelProvider','ProviderCredential','Workspace','WorkspaceMembership')}
  for name,manager in managers.items():manager.create.return_value=types.SimpleNamespace(id='fixture-'+name)
  managers['Workspace'].exists.return_value=False
  models=types.SimpleNamespace(**{name:types.SimpleNamespace(objects=manager) for name,manager in managers.items()})
  user=types.SimpleNamespace(objects=Mock());user.objects.create_user.return_value=types.SimpleNamespace(id='fixture-user')
  modules={'django.conf':types.SimpleNamespace(settings=types.SimpleNamespace(DATABASES={'default':{'NAME':'retained_acceptance'}},MEDIA_ROOT=folder)),
    'django.contrib.auth':types.SimpleNamespace(get_user_model=lambda:user),
    'django.db':types.SimpleNamespace(connection=object(),transaction=types.SimpleNamespace(atomic=contextlib.nullcontext)),
    'app_core.credentials':types.SimpleNamespace(encrypt_credential_secret=lambda value:'encrypted-synthetic'), 'app_core.models':models}
  environment={'AGENT_RUN_EXECUTION_MODE':mode,'CENTAERIS_ACCEPTANCE_SHARED_WORKLOAD':'1' if shared else '0',
    'CENTAERIS_ACCEPTANCE_WORKSPACE_RECOVERY':'1' if recovery else '0'}
  with patch.dict(sys.modules,modules),patch.dict(os.environ,environment),patch('builtins.print') as printed:
   exec(compile(Path(__file__).with_name('seed.py').read_text(),'seed.py','exec'),{})
  value=json.loads((Path(folder)/'acceptance-seed.json').read_text())
  self.assertNotIn(value['password'],str(printed.call_args_list))
  self.model_name=managers['ModelConfig'].create.call_args.kwargs['modelName']
  return value
 def test_recovery_seed_selects_fixed_recovery_protocol_only_for_explicit_per_run_shared_fixture(self):
  with tempfile.TemporaryDirectory() as folder:
   self.execute(folder,'perRun',recovery=True)
   self.assertEqual(self.model_name,'perf-workspace-recovery')
  with tempfile.TemporaryDirectory() as folder:
   with self.assertRaises(AssertionError):self.execute(folder,'retained',recovery=True)
 def test_shared_seed_records_the_explicit_per_run_or_retained_mode(self):
  for mode in ('perRun','retained'):
   with self.subTest(mode=mode),tempfile.TemporaryDirectory() as folder:
    self.assertEqual(self.execute(folder,mode)['executionMode'],mode)
 def test_rotation_seed_keeps_retained_and_rejects_missing_or_unsupported_modes(self):
  with tempfile.TemporaryDirectory() as folder:self.assertEqual(self.execute(folder,'retained',shared=False)['executionMode'],'retained')
  for mode,shared in (('',True),('unknown',True),('perRun',False)):
   with self.subTest(mode=mode,shared=shared),tempfile.TemporaryDirectory() as folder:
    with self.assertRaises(AssertionError):self.execute(folder,mode,shared)
    self.assertFalse((Path(folder)/'acceptance-seed.json').exists())

if __name__=='__main__':unittest.main()
