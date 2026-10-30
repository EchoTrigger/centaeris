import copy, importlib.util, unittest
from pathlib import Path
spec=importlib.util.spec_from_file_location('shared_workload',Path(__file__).with_name('shared_workload.py'))
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
class SharedWorkloadTests(unittest.TestCase):
 def rows(self):
  return [{'agentRunId':f'run-{e}-{i}','sessionId':f'session-{e}','environmentId':f'environment-{e}',
   'instanceId':f'instance-{e}','runStatus':'completed','coreTerminalType':'agent_run_completed',
   'cleanupComplete':True,'releaseReceiptCount':1} for e in range(4) for i in range(3)]
 def test_all_twelve_runs_keep_four_independent_original_instances(self):
  self.assertEqual(module.validate_run_facts(self.rows(),4,3),{'completedRuns':12,'sameInstanceEnvironments':4,'cleanupCompleteRuns':12})
 def test_completed_label_without_cleanup_or_exact_terminal_is_rejected(self):
  for key,value in [('cleanupComplete',False),('coreTerminalType',None),('releaseReceiptCount',0),('runStatus','failed')]:
   rows=self.rows();rows[0][key]=value
   with self.assertRaises(ValueError):module.validate_run_facts(rows,4,3)
 def test_cross_session_environment_and_replaced_instance_are_rejected(self):
  for key,value in [('environmentId','environment-1'),('instanceId','replaced-instance')]:
   rows=self.rows();rows[0][key]=value
   with self.assertRaises(ValueError):module.validate_run_facts(rows,4,3)
 def test_missing_or_duplicate_runs_are_rejected(self):
  for rows in (self.rows()[:-1],self.rows()+[self.rows()[0]]):
   with self.assertRaises(ValueError):module.validate_run_facts(rows,4,3)
class DirectCliBootstrapTests(unittest.TestCase):
 def test_real_driver_reaches_owner_guard_from_direct_cli(self):
  import json,os,subprocess,sys,tempfile
  with tempfile.TemporaryDirectory() as directory:
   root=Path(directory);fixture=root/'fixture';fixture.mkdir();owner=root/'owner.json'
   owner.write_text(json.dumps({'privateContainerdVerified':False}))
   project='retainedaccept'+'a'*10
   (fixture/'manifest.json').write_text(json.dumps({'project':project,'isolationOwner':str(owner)}))
   (fixture/'compose.json').write_text(json.dumps({'name':project}))
   environment={key:value for key,value in os.environ.items() if key!='PYTHONPATH'}
   result=subprocess.run([sys.executable,str(Path(__file__).with_name('shared_workload.py')),'--fixture',str(fixture),'--result',str(root/'result'),'--k6-binary',str(root/'not-executed'),'--run-terminal-timeout-seconds','300'],cwd=root,env=environment,capture_output=True,text=True,timeout=15)
   self.assertNotEqual(result.returncode,0)
   self.assertIn('private transport and raw OCI root must be observed first',result.stderr)

if __name__=='__main__':unittest.main()
