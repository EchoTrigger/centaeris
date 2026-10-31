import hashlib,importlib.util,json,subprocess,sys,tempfile,unittest
from pathlib import Path
from unittest.mock import patch
with patch.object(sys,'path',[str(Path(__file__).parent),*sys.path]):
 spec=importlib.util.spec_from_file_location('native_prepare',Path(__file__).with_name('prepare.py'));prepare=importlib.util.module_from_spec(spec);spec.loader.exec_module(prepare)
class PreparationTests(unittest.TestCase):
 def test_recovery_preparation_records_explicit_protocol_and_preserves_existing_shared_fixture(self):
  with tempfile.TemporaryDirectory() as tmp:
   root=Path(tmp);argv=self.fixture(root)+['--shared-workload','--environment-count','2','--workspace-recovery']
   model=root/'source/perf/mock-model';model.mkdir(parents=True)
   for name in ('server.py','fixed_workload.py'):(model/name).write_text('# deterministic test input')
   with patch.object(sys,'argv',argv),patch.object(prepare,'verify_owner',return_value={'root':'/run/owned'}),patch.object(prepare.subprocess,'run',side_effect=self.outputs()),patch('builtins.print'):prepare.main()
   manifest=json.loads((root/'result/manifest.json').read_text())
   env=dict(v.split('=',1) for v in (root/'result/synthetic.env').read_text().splitlines())
   self.assertIs(manifest['workspaceRecovery'],True)
   self.assertEqual(env['CENTAERIS_ACCEPTANCE_WORKSPACE_RECOVERY'],'1')
 def fixture(self,root):
  source=root/'source';source.mkdir();(source/'.env.example').write_text('EXECUTION_GLOBAL_LIMIT=8\nWORKER_SLOT_COUNT=8\nRUNTIME_NATIVE_SHARED_ADMISSION=1\nRESIDENT_GLOBAL_PIDS=8192\nRESIDENT_TENANT_PIDS=2048\n')
  bins=root/'bins';bins.mkdir()
  for name in ('runtime_server','execution_agent','environment_host_supervisor'):(bins/name).write_bytes(name.encode())
  manifest=root/'build.json';manifest.write_text(json.dumps({'sourceRevision':'a'*40,'sourceTree':'c'*40,'binarySha256':{n:hashlib.sha256(n.encode()).hexdigest() for n in ('runtime_server','execution_agent')},'hostSupervisorSha256':hashlib.sha256(b'environment_host_supervisor').hexdigest()}))
  tls=root/'tls';tls.mkdir()
  for name in ('model.key','model.crt','combined-ca.pem'):(tls/name).write_text('synthetic fixture certificate')
  return ['prepare','--source',str(source),'--bins',str(bins),'--output',str(root/'result'),'--revision','a'*40,'--build-manifest',str(manifest),'--tls-dir',str(tls),'--isolation-owner',str(root/'owner.json'),'--runsc-rotation','--docker-socket','/run/owned/docker.sock','--host-socket-directory','/run/owned/host']+[v for n in ('api','worker','runtime','general','postgres','redis') for v in ('--'+n+'-image','cached-image')]
 def outputs(self):return [subprocess.CompletedProcess([],0,'','')]+[subprocess.CompletedProcess([],0,'sha256:'+'b'*64,'')]*6+[subprocess.CompletedProcess([],0,'a'*40,''),subprocess.CompletedProcess([],0,'',''),subprocess.CompletedProcess([],0,'c'*40,''),subprocess.CompletedProcess([],0,str(16*1024**3),'')]
 def test_native_fixture_preserves_resource_charge_and_splits_maintenance_authority(self):
  with tempfile.TemporaryDirectory() as tmp:
   root=Path(tmp);argv=self.fixture(root)+['--execution-mode','retained']
   with patch.object(sys,'argv',argv),patch.object(prepare,'verify_owner',return_value={'root':'/run/owned'}) as owner,patch.object(prepare.subprocess,'run',side_effect=self.outputs()) as run,patch('builtins.print') as printed:prepare.main()
   config=json.loads((root/'result/compose.json').read_text());manifest=json.loads((root/'result/manifest.json').read_text());env=dict(v.split('=',1) for v in (root/'result/synthetic.env').read_text().splitlines())
   self.assertEqual(env['RUNTIME_NATIVE_SHARED_ADMISSION'],'0')
   self.assertEqual(env['RUNTIME_RUNSC_MAX_RUNNING_ENVIRONMENTS'],'1');self.assertEqual(env['RUNTIME_RUNSC_IDLE_PAUSE_MS'],'3000');self.assertEqual(env['SANDBOX_PIDS_LIMIT'],'512');self.assertEqual(env['RESIDENT_GLOBAL_COUNT'],'2');self.assertEqual(env['WORKER_SLOT_COUNT'],'2');self.assertEqual(env['EXECUTION_GLOBAL_LIMIT'],'2')
   self.assertEqual(config['services']['runtime']['user'],'0:0');self.assertIn('/run/owned/host:/run/centaeris-host:ro',config['services']['runtime']['volumes'])
   helper=str((root/'bins/execution_agent').resolve());self.assertIn(helper+':'+helper+':ro',config['services']['runtime']['volumes'])
   self.assertEqual((root/'result/general-image/execution_agent').read_bytes(),b'execution_agent')
   for name in ('api','worker','catalog-init'):
    effective={}
    for file in config['services'][name]['env_file']:effective.update(line.split('=',1) for line in Path(file).read_text().splitlines())
    self.assertNotIn('ENVIRONMENT_MAINTENANCE_TOKEN',effective)
   self.assertEqual(manifest['isolationOwner'],str((root/'owner.json').resolve()))
   self.assertNotIn(env['INTERNAL_API_TOKEN'],str(printed.call_args_list));self.assertEqual(run.call_count,10);owner.assert_called_once()
 def test_shared_workload_uses_default_caps_and_overlapping_slots_with_owned_model(self):
  with tempfile.TemporaryDirectory() as tmp:
   root=Path(tmp);argv=self.fixture(root)+['--shared-workload','--environment-count','4']
   model=root/'source/perf/mock-model';model.mkdir(parents=True)
   for name in ('server.py','fixed_workload.py'):(model/name).write_text('# deterministic test input')
   with patch.object(sys,'argv',argv),patch.object(prepare,'verify_owner',return_value={'root':'/run/owned'}),patch.object(prepare.subprocess,'run',side_effect=self.outputs()),patch('builtins.print'):prepare.main()
   config=json.loads((root/'result/compose.json').read_text());manifest=json.loads((root/'result/manifest.json').read_text());env=dict(v.split('=',1) for v in (root/'result/synthetic.env').read_text().splitlines())
   for name in ('WORKER_SLOT_COUNT','EXECUTION_GLOBAL_LIMIT','RESIDENT_GLOBAL_COUNT','RESIDENT_TENANT_COUNT','RUNTIME_RUNSC_MAX_RUNNING_ENVIRONMENTS'):self.assertEqual(env[name],'4')
   self.assertEqual(env['SANDBOX_MEMORY_BYTES'],str(4*1024**3));self.assertEqual(env['SANDBOX_CPU_MILLI'],'2000');self.assertEqual(env['SANDBOX_WORKSPACE_BYTES'],str(4*1024**3))
   self.assertEqual(env['WORKSPACE_PERF_OBSERVATIONS'],'1')
   self.assertEqual(env['RUNTIME_NATIVE_SHARED_ADMISSION'],'1');self.assertEqual(env['RESIDENT_TENANT_MEMORY_BYTES'],str(8*1024**3));self.assertEqual(env['RESIDENT_TENANT_WORKSPACE_BYTES'],str(32*1024**3))
   self.assertEqual(config['services']['model']['command'],['/app/.venv/bin/python','/harness/server.py'])
   self.assertEqual(manifest['executionMode'],'perRun');self.assertEqual(env['AGENT_RUN_EXECUTION_MODE'],'perRun')
   self.assertEqual(manifest['sharedWorkload']['environmentCount'],4)
   self.assertEqual(manifest['sharedWorkload']['workProfile'],{'memoryMiB':0,'minimumHoldMs':0})
   self.assertEqual((root/'tls/model.pem').read_text(),'synthetic fixture certificatesynthetic fixture certificate')
 def test_sparse_eight_keeps_full_caps_and_records_model_wait_stagger_and_overlap(self):
  with tempfile.TemporaryDirectory() as tmp:
   root=Path(tmp);argv=self.fixture(root)+['--shared-workload','--environment-count','8','--model-wait-ms','400','--initial-stagger-ms','125','--expected-working-environments','2']
   model=root/'source/perf/mock-model';model.mkdir(parents=True)
   for name in ('server.py','fixed_workload.py'):(model/name).write_text('# deterministic test input')
   with patch.object(sys,'argv',argv),patch.object(prepare,'verify_owner',return_value={'root':'/run/owned'}),patch.object(prepare.subprocess,'run',side_effect=self.outputs()),patch('builtins.print'):prepare.main()
   config=json.loads((root/'result/compose.json').read_text());manifest=json.loads((root/'result/manifest.json').read_text());env=dict(v.split('=',1) for v in (root/'result/synthetic.env').read_text().splitlines())
   self.assertEqual(config['services']['model']['environment']['MOCK_FIXED_MODEL_WAIT_MS'],'400')
   self.assertEqual(manifest['sharedWorkload']['modelWaitMs'],400)
   self.assertEqual(manifest['sharedWorkload']['initialStaggerMs'],125)
   self.assertEqual(manifest['sharedWorkload']['expectedWorkingEnvironments'],2)
   self.assertEqual(manifest['sharedWorkload']['environmentCount'],8)
   self.assertEqual(env['RESIDENT_TENANT_MEMORY_BYTES'],str(12*1024**3))
   self.assertEqual(env['RESIDENT_GLOBAL_MEMORY_BYTES'],str(12*1024**3))
   self.assertEqual(env['SANDBOX_MEMORY_BYTES'],str(4*1024**3));self.assertEqual(env['SANDBOX_CPU_MILLI'],'2000')
   self.assertEqual(env['RUNTIME_RUNSC_MAX_RUNNING_ENVIRONMENTS'],'8')
 def test_dense_profile_changes_only_tenant_memory_budget_and_records_profile(self):
  with tempfile.TemporaryDirectory() as tmp:
   root=Path(tmp);argv=self.fixture(root)+['--shared-workload','--environment-count','8','--iterations','1','--memory-mib','1536','--minimum-hold-ms','60000','--workspace-memory-budget-bytes',str(8*1024**3)]
   model=root/'source/perf/mock-model';model.mkdir(parents=True)
   for name in ('server.py','fixed_workload.py'):(model/name).write_text('# deterministic test input')
   with patch.object(sys,'argv',argv),patch.object(prepare,'verify_owner',return_value={'root':'/run/owned'}),patch.object(prepare.subprocess,'run',side_effect=self.outputs()),patch('builtins.print'):prepare.main()
   config=json.loads((root/'result/compose.json').read_text());manifest=json.loads((root/'result/manifest.json').read_text());env=dict(v.split('=',1) for v in (root/'result/synthetic.env').read_text().splitlines())
   self.assertEqual(env['RESIDENT_TENANT_MEMORY_BYTES'],str(8*1024**3));self.assertEqual(env['RESIDENT_GLOBAL_MEMORY_BYTES'],str(12*1024**3))
   self.assertEqual(env['SANDBOX_MEMORY_BYTES'],str(4*1024**3));self.assertEqual(env['SANDBOX_CPU_MILLI'],'2000')
   self.assertEqual(manifest['sharedWorkload']['workspaceMemoryBudgetBytes'],8*1024**3)
   self.assertEqual(manifest['sharedWorkload']['workProfile'],{'memoryMiB':1536,'minimumHoldMs':60000})
   self.assertEqual(env['RESIDENT_HOST_MEMORY_HEADROOM_BYTES'],str((16*1024**3)//5))
   self.assertEqual(manifest['resourceLimits'],{'global':{'count':8,'memoryBytes':12*1024**3,'pids':8192,'workspaceBytes':32*1024**3},'tenant':{'count':8,'memoryBytes':8*1024**3,'pids':2048,'workspaceBytes':32*1024**3},'hostHeadroomBytes':(16*1024**3)//5,'requested':{'memoryBytes':4*1024**3,'pids':512,'workspaceBytes':4*1024**3}})
   self.assertEqual(config['services']['model']['environment'],{'MOCK_TLS_CERT':'/harness/model.crt','MOCK_TLS_KEY':'/harness/model.key','MOCK_FIXED_MODEL_WAIT_MS':'0'})
 def test_shared_resource_projection_uses_actual_pids_and_host_headroom_floor(self):
  with tempfile.TemporaryDirectory() as tmp:
   root=Path(tmp);argv=self.fixture(root)+['--shared-workload']
   (root/'source/.env.example').write_text('RESIDENT_GLOBAL_PIDS=6144\nRESIDENT_TENANT_PIDS=3072\n')
   model=root/'source/perf/mock-model';model.mkdir(parents=True)
   for name in ('server.py','fixed_workload.py'):(model/name).write_text('# deterministic test input')
   outputs=self.outputs();outputs[-1]=subprocess.CompletedProcess([],0,str(1024**3),'')
   with patch.object(sys,'argv',argv),patch.object(prepare,'verify_owner',return_value={'root':'/run/owned'}),patch.object(prepare.subprocess,'run',side_effect=outputs),patch('builtins.print'):prepare.main()
   manifest=json.loads((root/'result/manifest.json').read_text())
   self.assertEqual(manifest['resourceLimits']['global']['pids'],6144)
   self.assertEqual(manifest['resourceLimits']['tenant']['pids'],3072)
   self.assertEqual(manifest['resourceLimits']['hostHeadroomBytes'],512*1024**2)
 def test_shared_fixture_rejects_invalid_host_capacity_before_creating_output(self):
  for value in ('0','-1','true','null','"invalid"'):
   with self.subTest(value=value),tempfile.TemporaryDirectory() as tmp:
    root=Path(tmp);argv=self.fixture(root)+['--shared-workload'];outputs=self.outputs();outputs[-1]=subprocess.CompletedProcess([],0,value,'')
    with patch.object(sys,'argv',argv),patch.object(prepare,'verify_owner',return_value={'root':'/run/owned'}),patch.object(prepare.subprocess,'run',side_effect=outputs):
     with self.assertRaisesRegex(SystemExit,'positive Docker host memory'):prepare.main()
    self.assertFalse((root/'result').exists())
 def test_prepare_rejects_invalid_profiles_and_budgets_before_isolation_or_mutation(self):
  cases=[([],['--memory-mib','1','--minimum-hold-ms','1'],'require shared mode'),
   ([],['--workspace-memory-budget-bytes',str(8*1024**3)],'require shared mode'),
   (['--shared-workload'],['--memory-mib','1'],'paired'),
   (['--shared-workload','--iterations','2'],['--memory-mib','1','--minimum-hold-ms','1'],'one iteration'),
   (['--shared-workload','--environment-count','1','--iterations','1'],['--memory-mib','1','--minimum-hold-ms','1'],'two environments'),
   (['--shared-workload'],['--workspace-memory-budget-bytes',str(12*1024**3+1)],'workspace memory budget')]
  for shared,options,message in cases:
   with self.subTest(options=options),tempfile.TemporaryDirectory() as tmp:
    root=Path(tmp);argv=self.fixture(root)+['--execution-mode','retained']+shared+options
    with patch.object(sys,'argv',argv),patch.object(prepare,'verify_owner') as owner,patch.object(prepare.subprocess,'run') as run,patch.object(sys,'stderr') as stderr:
     with self.assertRaises(SystemExit):prepare.main()
    self.assertIn(message,''.join(str(call.args[0]) for call in stderr.write.call_args_list));owner.assert_not_called();run.assert_not_called();self.assertFalse((root/'result').exists())
 def test_rotation_gate_requires_explicit_retained_mode(self):
  with tempfile.TemporaryDirectory() as tmp:
   root=Path(tmp);argv=self.fixture(root)
   with patch.object(sys,'argv',argv),patch.object(prepare,'verify_owner') as owner,patch.object(sys,'stderr') as stderr:
    with self.assertRaises(SystemExit):prepare.main()
   self.assertIn('rotation requires retained',''.join(str(call.args[0]) for call in stderr.write.call_args_list));owner.assert_not_called()
 def test_unverified_isolation_rejects_before_any_docker_or_file_mutation(self):
  with tempfile.TemporaryDirectory() as tmp:
   root=Path(tmp);argv=self.fixture(root)+['--execution-mode','retained']
   with patch.object(sys,'argv',argv),patch.object(prepare,'verify_owner',side_effect=RuntimeError('shared containerd rejected')),patch.object(prepare.subprocess,'run') as run:
    with self.assertRaisesRegex(RuntimeError,'shared containerd'):prepare.main()
   run.assert_not_called();self.assertFalse((root/'result').exists())
 def test_foreign_managed_responsibility_rejects_without_starting_services(self):
  with tempfile.TemporaryDirectory() as tmp:
   root=Path(tmp);argv=self.fixture(root)+['--execution-mode','retained']
   with patch.object(sys,'argv',argv),patch.object(prepare,'verify_owner',return_value={'root':'/run/owned'}),patch.object(prepare.subprocess,'run',return_value=subprocess.CompletedProcess([],0,'foreign-id','')) as run:
    with self.assertRaisesRegex(SystemExit,'responsibility'):prepare.main()
   self.assertEqual(run.call_count,1);self.assertFalse((root/'result').exists())
if __name__=='__main__':unittest.main()
