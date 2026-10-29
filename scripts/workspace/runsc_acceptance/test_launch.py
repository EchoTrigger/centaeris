import importlib.util,struct,tempfile,unittest
from pathlib import Path
spec=importlib.util.spec_from_file_location('native_launch',Path(__file__).with_name('launch.py'));launch=importlib.util.module_from_spec(spec);spec.loader.exec_module(launch)
class StaticHelperTests(unittest.TestCase):
 def elf(self,program_type):
  identity=b'\x7fELF\x02\x01'+bytes(10)
  header=struct.pack('<16sHHIQQQIHHHHHH',identity,2,62,1,0,64,0,0,64,56,1,0,0,0)
  return header+struct.pack('<IIQQQQQQ',program_type,0,0,0,0,0,0,0)
 def test_static_elf_program_headers_accepted_without_readelf_dependency(self):
  with tempfile.TemporaryDirectory() as tmp:
   path=Path(tmp)/'helper';path.write_bytes(self.elf(1));launch.static_helper(path)
 def test_dynamic_or_invalid_elf_rejected_before_any_daemon_start(self):
  for data in (self.elf(3),b'invalid'):
   with self.subTest(data=data[:6]),tempfile.TemporaryDirectory() as tmp:
    path=Path(tmp)/'helper';path.write_bytes(data)
    with self.assertRaises(RuntimeError):launch.static_helper(path)
class SharedArgumentTests(unittest.TestCase):
 def test_recovery_launcher_rejects_non_per_run_or_non_shared_fixture_before_starting_anything(self):
  import subprocess,sys
  for options in ([],['--shared-workload','--execution-mode','retained']):
   command=[sys.executable,str(Path(__file__).with_name('launch.py')),'--root','absent','--source','absent','--workspace-recovery']+options
   for name in ('api','worker','runtime','general','postgres','redis'):command+=['--'+name+'-image','not-loaded']
   result=subprocess.run(command,capture_output=True,text=True,timeout=10)
   self.assertEqual(result.returncode,2)
   self.assertIn('workspace recovery requires explicit shared perRun fixture',result.stderr)
 def test_sparse_eight_parameters_are_forwarded_explicitly(self):
  self.assertEqual(launch.shared_arguments(8,400,125,2),['--model-wait-ms','400','--initial-stagger-ms','125','--expected-working-environments','2'])
  self.assertEqual(launch.shared_arguments(4),['--model-wait-ms','0','--initial-stagger-ms','0','--expected-working-environments','4'])
 def test_invalid_shared_budgets_reject_before_launch(self):
  for values in ((8,120001,0,None),(8,0,20000,None),(8,0,0,9),(8,0,0,0)):
   with self.subTest(values=values),self.assertRaises(ValueError):launch.shared_arguments(*values)
class MemoryProfileArgumentTests(unittest.TestCase):
 def test_dense_memory_profile_is_forwarded_and_default_shared_arguments_are_unchanged(self):
  self.assertEqual(launch.shared_arguments(8,0,10000,2,memory_mib=1536,minimum_hold_ms=60000,iterations=1),
   ['--model-wait-ms','0','--initial-stagger-ms','10000','--expected-working-environments','2','--memory-mib','1536','--minimum-hold-ms','60000'])
 def test_launcher_rejects_invalid_memory_configuration_before_loading_fixture(self):
  import subprocess,sys
  cases=[(['--memory-mib','1536'],'paired'),(['--minimum-hold-ms','60000'],'paired'),
   (['--memory-mib','1537','--minimum-hold-ms','60000'],'bounded'),
   (['--memory-mib','1','--minimum-hold-ms','120001'],'bounded'),
   (['--memory-mib','1','--minimum-hold-ms','1','--iterations','2'],'one iteration'),
   (['--workspace-memory-budget-bytes',str(4*1024**3-1)],'workspace memory budget'),
   (['--workspace-memory-budget-bytes',str(12*1024**3+1)],'workspace memory budget')]
  for options,message in cases:
   with self.subTest(options=options):
    command=[sys.executable,str(Path(__file__).with_name('launch.py')),'--root','absent','--source','absent','--shared-workload','--iterations','1']+options
    for name in ('api','worker','runtime','general','postgres','redis'):command+=['--'+name+'-image','not-loaded']
    result=subprocess.run(command,capture_output=True,text=True,timeout=10)
    self.assertEqual(result.returncode,2);self.assertIn(message,result.stderr)
 def test_launcher_rejects_memory_options_outside_shared_mode(self):
  import subprocess,sys
  for options in (['--memory-mib','1','--minimum-hold-ms','1'],['--workspace-memory-budget-bytes',str(8*1024**3)]):
   command=[sys.executable,str(Path(__file__).with_name('launch.py')),'--root','absent','--source','absent']+options
   for name in ('api','worker','runtime','general','postgres','redis'):command+=['--'+name+'-image','not-loaded']
   result=subprocess.run(command,capture_output=True,text=True,timeout=10)
   self.assertEqual(result.returncode,2);self.assertIn('require shared mode',result.stderr)
class FailureDiagnosticTests(unittest.TestCase):
 def test_failed_process_keeps_one_bounded_private_redacted_diagnostic(self):
  import json,os,stat,sys
  with tempfile.TemporaryDirectory() as directory:
   root=Path(directory);(root/'fixture').mkdir()
   known='synthetic-only-value-4938';other='another-fixture-value-6549'
   (root/'fixture/synthetic.env').write_text('INTERNAL_API_TOKEN='+known+'\nCONFIG_VALUE='+other+'\n')
   diagnostic=root/'launch-step-failure.json';diagnostic.write_text('obsolete failure detail')
   program='import sys;print("x"*20000);print("last safe context");print('+repr(known)+');print('+repr(other)+');print("PASSWORD unlisted-private-value");sys.stderr.write("safe trace\\n"+'+repr(known)+');sys.exit(7)'
   with self.assertRaisesRegex(RuntimeError,'owned fixture step failed') as raised:
    launch.run_step([sys.executable,'-c',program],root=root,timeout=5,capture_bytes=4096,diagnostic_bytes=1024)
   data=diagnostic.read_text();value=json.loads(data)
   self.assertNotIn(known,data);self.assertNotIn(other,data);self.assertNotIn('unlisted-private-value',data)
   self.assertNotIn('obsolete failure detail',data);self.assertNotIn(known,str(raised.exception))
   self.assertIn('last safe context',value['stdout']);self.assertIn('safe trace',value['stderr'])
   self.assertEqual(value['returnCode'],7);self.assertTrue(value['stdoutTruncated'])
   self.assertLess(diagnostic.stat().st_size,4000)
   self.assertEqual(list(root.glob('*failure*.json')),[diagnostic])
   if os.name=='posix':self.assertEqual(stat.S_IMODE(diagnostic.stat().st_mode),0o600)
 def test_success_returns_stdout_without_persisting_process_output(self):
  import sys
  with tempfile.TemporaryDirectory() as directory:
   root=Path(directory)
   self.assertEqual(launch.run_step([sys.executable,'-c','print("bounded result")'],root=root,timeout=5),'bounded result\n')
   self.assertFalse((root/'launch-step-failure.json').exists())
 def test_timeout_is_bounded_and_does_not_expose_command_arguments(self):
  import json,sys
  with tempfile.TemporaryDirectory() as directory:
   root=Path(directory);(root/'fixture').mkdir();known='timeout-private-value-9281'
   (root/'fixture/synthetic.env').write_text('INTERNAL_API_TOKEN='+known+'\n')
   with self.assertRaisesRegex(RuntimeError,'owned fixture step failed') as raised:
    launch.run_step([sys.executable,'-c','import time;time.sleep(10)',known],root=root,timeout=.1)
   report=json.loads((root/'launch-step-failure.json').read_text())
   self.assertEqual(report['failureType'],'timeout');self.assertNotIn(known,str(raised.exception))

class StreamDeadlineArgumentTests(unittest.TestCase):
 def test_default_and_explicit_stream_deadlines_are_bounded(self):
  self.assertEqual(launch.terminal_timeout('120'),120)
  self.assertEqual(launch.terminal_timeout('300'),300)
  for value in ('0','-1','601','NaN','1.5'):
   with self.assertRaises(ValueError):launch.terminal_timeout(value)

class BetweenRunArgumentTests(unittest.TestCase):
 def test_private_pause_probe_gap_remains_explicit_and_bounded(self):
  for value in ('0','6000','30000','60000'):self.assertEqual(launch.between_run_delay(value),int(value))
  for value in ('-1','60001','NaN','1.5'):
   with self.assertRaises(ValueError):launch.between_run_delay(value)

class WorkloadBudgetTests(unittest.TestCase):
 def test_four_by_three_has_a_derived_budget_for_every_existing_serial_phase(self):
  budget=launch.workload_budget(4,3,300,30000,0)
  self.assertEqual(budget['k6'],{'scenarioSeconds':1230,'setupSeconds':180,'gracefulStopSeconds':30})
  self.assertEqual(budget['k6ProcessSeconds'],1465)
  self.assertEqual(budget['completedReadbackSeconds'],12*181)
  self.assertEqual(budget['finalPauseSeconds'],4*241)
  self.assertEqual(budget['retirementSeconds'],4*391)
  self.assertEqual(budget['coordinatorSeconds'],6566)
  self.assertEqual(budget['limitSeconds'],14400)
 def test_eight_environment_stagger_is_budgeted_without_lowering_overlap_or_run_count(self):
  budget=launch.workload_budget(8,3,300,30000,3000)
  self.assertEqual(budget['k6']['scenarioSeconds'],1251)
  self.assertEqual(budget['coordinatorSeconds'],11287)
 def test_per_run_budget_covers_each_receipt_inventory_and_public_history_without_retained_actions(self):
  budget=launch.workload_budget(4,3,300,30000,0,execution_mode='perRun')
  self.assertEqual(budget['executionMode'],'perRun')
  self.assertEqual(budget['finalPauseSeconds'],0);self.assertEqual(budget['retirementSeconds'],0)
  self.assertEqual(budget['perRunCleanupReadbackSeconds'],12*(60+60+30+30))
  self.assertEqual(budget['finalManagedInventorySeconds'],60)
 def test_launcher_rejects_the_total_budget_before_reading_fixture_or_k6(self):
  import subprocess,sys
  with tempfile.TemporaryDirectory() as directory:
   absent=Path(directory)/'not-created'
   command=[sys.executable,str(Path(__file__).with_name('launch.py')),'--root',str(absent),'--source',str(absent),
    '--shared-workload','--execute','--environment-count','64','--iterations','20','--work-units','100000',
    '--run-terminal-timeout-seconds','600','--between-run-ms','60000','--k6-binary',str(absent)]
   for name in ('api','worker','runtime','general','postgres','redis'):command+=['--'+name+'-image','not-loaded']
   result=subprocess.run(command,capture_output=True,text=True,timeout=10)
   self.assertEqual(result.returncode,2)
   self.assertIn('four-hour',result.stderr)
   self.assertFalse(absent.exists())
 def test_excessive_combination_rejects_before_linux_fixture_or_k6_is_loaded(self):
  with self.assertRaisesRegex(ValueError,'four.hour'):
   launch.workload_budget(64,20,600,60000,0)



class StorageDriverArgumentTests(unittest.TestCase):
 def test_default_and_explicit_storage_driver_reach_the_owned_daemon_without_starting_it(self):
  import hashlib,json,sys,types
  from unittest.mock import patch
  class StoppedBeforeDaemonStart(Exception):pass
  for extra,expected in (([],'overlay2'),(['--storage-driver','overlay2'],'overlay2')):
   with self.subTest(expected=expected),tempfile.TemporaryDirectory(dir='/var/tmp',prefix='centaeris-runsc-storage-test-') as tmp:
    root=Path(tmp);source=root/'source';source.mkdir();bins=root/'bins';bins.mkdir()
    values={'runtime_server':b'fixture-runtime','execution_agent':StaticHelperTests().elf(1),'environment_host_supervisor':b'fixture-supervisor'}
    for name,value in values.items():(bins/name).write_bytes(value)
    (root/'build-final.json').write_text(json.dumps({'binarySha256':{name:hashlib.sha256(value).hexdigest() for name,value in values.items()},'hostSupervisorSha256':hashlib.sha256(values['environment_host_supervisor']).hexdigest()}))
    argv=['launch','--root',str(root),'--source',str(source),'--execution-mode','retained','--execute']+extra
    for name in ('api','worker','runtime','general','postgres','redis'):argv+=['--'+name+'-image','sha256:'+'a'*64]
    original_stat=Path.stat
    def stat(path,**kwargs):
     if str(path)=='/proc/self/ns/net':return types.SimpleNamespace(st_ino=2)
     if str(path)=='/proc/1/ns/net':return types.SimpleNamespace(st_ino=1)
     return original_stat(path,**kwargs)
    def command(argv,**kwargs):
     if argv==['ip','link','set','lo','up']:return ''
     if argv==['ip','-j','link']:return json.dumps([{'ifname':'lo'}])
     raise AssertionError('unexpected command before private daemon')
    with patch.object(sys,'argv',argv),patch.object(launch.os,'geteuid',return_value=0),patch.object(Path,'stat',stat),patch.object(launch,'command',side_effect=command),patch.object(launch,'PrivateDocker',side_effect=StoppedBeforeDaemonStart) as constructor:
     with self.assertRaises(StoppedBeforeDaemonStart):launch.main()
    constructor.assert_called_once_with(root.resolve(),network=True,storage_driver=expected)
 def test_unsupported_storage_driver_fails_before_fixture_access(self):
  import subprocess,sys
  argv=[sys.executable,str(Path(__file__).with_name('launch.py')),'--root','absent','--source','absent','--storage-driver','vfs']
  for name in ('api','worker','runtime','general','postgres','redis'):argv+=['--'+name+'-image','not-loaded']
  result=subprocess.run(argv,capture_output=True,text=True,timeout=10)
  self.assertEqual(result.returncode,2);self.assertIn('invalid choice',result.stderr)

if __name__=='__main__':unittest.main()
