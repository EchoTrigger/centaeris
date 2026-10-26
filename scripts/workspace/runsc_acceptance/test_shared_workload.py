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
if __name__=='__main__':unittest.main()
