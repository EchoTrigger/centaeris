import hashlib,importlib.util,json,shlex,sys,tempfile,unittest
from pathlib import Path
from unittest.mock import patch
sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
from runsc_acceptance import daemon
class RawProbeTests(unittest.TestCase):
 def fake(self,root,replace=False):
  class Docker(daemon.PrivateDocker):
   def __init__(self):
    self.root=root;self.namespace='runsc-'+'a'*12;self.owner={'fixtureOwner':'a'*32,'runscVersion':['test-version'],'runscArguments':['--platform=ptrace']};self.paused=False;self.removed=False;self.calls=[]
    (root/'exec/runtime-runc'/self.namespace).mkdir(parents=True)
   def run(self,*args,timeout=30):
    self.calls.append(args)
    if args[0]=='run':return '1'*64
    if args[0]=='pause':self.paused=True;return ''
    if args[0]=='unpause':self.paused=False;return ''
    if args[0]=='rm':self.removed=True;return ''
    if args[0]=='ps':return '' if self.removed else '1'*64
    if args[0]=='exec':
     if self.paused:raise RuntimeError('task RPC attempted while paused')
     challenge=shlex.split(args[-1])[-1];index=int(challenge.rsplit('-',1)[1]);nonce=('b' if replace and index else 'a')*64
     value={'schema':'runsc.rotation.oracle.v1','nonce':nonce,'pid':42,'startTicks':9,'counter':index+1,'fileSha256':'c'*64,'challenge':challenge,'proof':hashlib.sha256((nonce+':'+challenge).encode()).hexdigest()}
     return 'RUNSC_ROTATION_ORACLE '+json.dumps(value)
    raise AssertionError(args)
   def metrics(self,cid,pid,limit):return {'current':23,'max':limit,'eventsMax':0}
   def inspect(self,cid):return {'Id':cid,'HostConfig':{'Runtime':'runsc'},'State':{'Running':True,'Paused':self.paused,'Pid':321},'Config':{'Labels':{'centaeris.acceptance.owner':self.owner['fixtureOwner']}}}
  return Docker()
 def test_twelve_cycles_use_actual_memory_challenges_and_same_physical_pid(self):
  with tempfile.TemporaryDirectory() as tmp:
   d=self.fake(Path(tmp))
   with patch.object(daemon,'command',return_value=json.dumps({'id':'1'*64,'status':'running'})),patch.object(daemon.time,'sleep'):proof=d.probe('image',12)
   self.assertEqual(len(proof['observations']),12);self.assertEqual(sum(c[0]=='pause' for c in d.calls),12);self.assertEqual(sum(c[0]=='unpause' for c in d.calls),12);self.assertTrue(d.owner['rawProbeRetired']);self.assertEqual(proof['namespace'],d.namespace)
 def test_new_nonce_rejects_before_product_use_and_keeps_probe_owned(self):
  with tempfile.TemporaryDirectory() as tmp:
   d=self.fake(Path(tmp),replace=True)
   with patch.object(daemon,'command',return_value=json.dumps({'id':'1'*64,'status':'running'})),patch.object(daemon.time,'sleep'),self.assertRaisesRegex(RuntimeError,'memory/file'):d.probe('image',12)
   self.assertFalse(d.removed);self.assertIn('rawProbeContainerId',d.owner)
 def test_only_supported_private_fatal_logging_is_selected(self):
  with tempfile.TemporaryDirectory() as tmp:
   arguments=daemon.runtime_logging(['--platform=ptrace','--in-sandbox-cgroup=v2'],Path(tmp),'  -panic-log string\n  -log string\n  -log-format string\n')
   self.assertIn('--panic-log='+str(Path(tmp)/'runsc-logs/panic.log'),arguments);self.assertIn('--platform=ptrace',arguments)
  with tempfile.TemporaryDirectory() as tmp,self.assertRaises(RuntimeError):daemon.runtime_logging([],Path(tmp),'unknown flags')
 def test_unknown_container_is_not_removed_when_no_project_was_registered(self):
  from unittest.mock import Mock
  d=daemon.PrivateDocker.__new__(daemon.PrivateDocker);d.owner={'fixtureOwner':'a'*32};d.daemon=Mock();d.containerd=Mock()
  d.run=Mock(side_effect=['','foreign-container']);d.inspect=Mock(return_value={'Config':{'Labels':{}}})
  with self.assertRaisesRegex(RuntimeError,'unknown'):d.close()
  d.daemon.terminate.assert_not_called();d.containerd.terminate.assert_not_called()
 def test_runtime_diagnostic_tail_is_bounded_and_credentials_suppressed(self):
  with tempfile.TemporaryDirectory() as tmp:
   root=Path(tmp);(root/'runsc-logs').mkdir();(root/'runsc-logs/panic.log').write_text('x'*100000+'\npanic: public-runtime-failure\npassword=private-test-value\n')
   d=daemon.PrivateDocker.__new__(daemon.PrivateDocker);d.root=root
   rows=d.capture_runtime_errors();self.assertIn('public-runtime-failure',str(rows));self.assertNotIn('private-test-value',str(rows));self.assertLess(len(str(rows)),70000)
 def test_installed_flag_descriptions_on_stderr_are_accepted(self):
  import subprocess
  help_text='  -panic-log string\n  -log string\n  -log-format string\n'
  with tempfile.TemporaryDirectory() as tmp,patch.object(daemon.subprocess,'run',return_value=subprocess.CompletedProcess([],0,'',help_text)):
   flags=daemon.runtime_flags('/usr/bin/runsc')
   selected=daemon.runtime_logging(['--platform=ptrace'],Path(tmp),flags)
   self.assertIn('--panic-log='+str(Path(tmp)/'runsc-logs/panic.log'),selected)
 def test_raw_pid_budget_matches_production_and_records_each_cycle(self):
  for limit in (512,256):
   with self.subTest(limit=limit),tempfile.TemporaryDirectory() as tmp:
    d=self.fake(Path(tmp))
    with patch.object(daemon,'command',return_value=json.dumps({'id':'1'*64,'status':'running'})),patch.object(daemon.time,'sleep'):
     proof=d.probe('image',2, pids_limit=limit)
    self.assertIn('--pids-limit='+str(limit),d.calls[0])
    self.assertEqual(proof['pidsLimit'],limit)
    self.assertEqual(proof['baselinePids']['max'],limit)
    self.assertTrue(all(r['pausedPids']['max']==limit and r['resumedPids']['eventsMax']==0 for r in proof['observations']))
 def test_network_preflight_rejects_host_namespace_before_starting_processes(self):
  with patch.object(daemon,'network_identity',side_effect=RuntimeError('private network namespace required')),patch.object(daemon.PrivateDocker,'shared_identity') as shared,patch.object(daemon.subprocess,'Popen') as popen:
   d=daemon.PrivateDocker.__new__(daemon.PrivateDocker)
   with self.assertRaisesRegex(RuntimeError,'private network'):d.start()
   popen.assert_not_called();shared.assert_not_called()
 def test_private_network_identity_must_differ_from_shared_daemon_and_init(self):
  for mapping in ({'self':1,'1':1,'240':2},{'self':2,'1':1,'240':2}):
   with self.subTest(mapping=mapping),self.assertRaisesRegex(RuntimeError,'private network'):daemon.network_identity(240,read_namespace=lambda key:mapping[str(key)])
  self.assertEqual(daemon.network_identity(240,read_namespace=lambda key:{'self':3,'1':1,'240':2}[str(key)]),{'own':3,'init':1,'sharedDocker':2})
 def test_owned_processes_must_inherit_the_verified_network_namespace(self):
  with self.assertRaisesRegex(RuntimeError,'private network'):daemon.network_identity(240,read_namespace=lambda key:{'self':3,'1':1,'240':2,'900':2}[str(key)],owned_pids=[900])
  self.assertEqual(daemon.network_identity(240,read_namespace=lambda key:{'self':3,'1':1,'240':2,'900':3}[str(key)],owned_pids=[900])['ownedProcesses'],[3])
 def test_panic_header_survives_a_long_stack_trace(self):
  with tempfile.TemporaryDirectory() as tmp:
   root=Path(tmp);(root/'runsc-logs').mkdir();(root/'runsc-logs/panic.log').write_text('panic: first-thread-EAGAIN\n'+'stack-frame\n'*6000+'final-frame\n')
   d=daemon.PrivateDocker.__new__(daemon.PrivateDocker);d.root=root
   rows=d.capture_runtime_errors();self.assertIn('first-thread-EAGAIN',str(rows));self.assertIn('final-frame',str(rows));self.assertLess(len(str(rows)),70000)
 def test_scoped_pid_metrics_reject_foreign_identity_and_read_exact_controller(self):
  with tempfile.TemporaryDirectory() as tmp:
   root=Path(tmp);proc=root/'proc';group=root/'cgroup';(proc/'321').mkdir(parents=True);cid='1'*64;scope=group/'system.slice'/('docker-'+cid+'.scope');scope.mkdir(parents=True)
   (proc/'321/cgroup').write_text('0::/system.slice/docker-'+cid+'.scope\n')
   for name,value in [('pids.current','63'),('pids.max','512'),('pids.events','max 0\n')]: (scope/name).write_text(value)
   self.assertEqual(daemon.cgroup_pids(cid,321,512,proc_root=proc,cgroup_root=group),{'current':63,'max':512,'eventsMax':0})
   with self.assertRaisesRegex(RuntimeError,'identity'):daemon.cgroup_pids('2'*64,321,512,proc_root=proc,cgroup_root=group)
   with self.assertRaisesRegex(RuntimeError,'limit'):daemon.cgroup_pids(cid,321,128,proc_root=proc,cgroup_root=group)
if __name__=='__main__':unittest.main()
