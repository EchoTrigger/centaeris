import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[1] / 'harness/fixture_workload.py'
class ExistingFixtureWorkloadTests(unittest.TestCase):
    def load(self):
        spec=importlib.util.spec_from_file_location('fixture_workload_subject',SOURCE)
        module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module
    def fixture(self, directory):
        samples=[{'sampleId':f'{i+1:032x}','operationId':f'{i+101:032x}','workUnits':2} for i in range(12)]
        return {'schema':'workspace.perf.fixture.v1','nativeManifest':str(Path(directory)/'native/fixture/manifest.json'),
                'apiBase':'http://127.0.0.1:18083','workspaceId':'workspace-owned','agentId':'agent-owned',
                'modelRefs':{'tools':'model-owned'},'account':{'email':'synthetic@localhost.invalid','password':'not-for-output'},'samples':samples}
    def test_prepared_fixture_targets_and_secrets_are_passed_to_k6_only_through_environment(self):
        module=self.load()
        with tempfile.TemporaryDirectory() as folder:
            path=Path(folder)/'perf-fixture.json';path.write_text(json.dumps(self.fixture(folder)));path.chmod(0o600)
            native={'apiBase':'http://127.0.0.1:18083','project':'retainedaccept'+'a'*10,'sourceRevision':'b'*40}
            with patch.object(module,'verify_native_fixture',return_value=native):value=module.read_fixture(path,environments=4,iterations=3)
            command,environment=module.k6_command(value,Path(folder)/'out',Path('/existing/k6'),environments=4,iterations=3,initial_stagger_ms=2000,run_terminal_timeout_seconds=300)
            self.assertEqual(environment['PERF_INITIAL_STAGGER_MS'],'2000')
            self.assertEqual(environment['PERF_RUN_TERMINAL_TIMEOUT_SECONDS'],'300')
            self.assertNotIn('not-for-output',' '.join(command));self.assertEqual(environment['PERF_ADMIN_PASSWORD'],'not-for-output')
            self.assertNotIn('INTERNAL_API_TOKEN',environment);self.assertEqual(environment['PERF_WORKSPACE_ID'],'workspace-owned')
            self.assertEqual(environment['PERF_SAMPLES_PATH'],str(Path(folder)/'out/samples.json'))
            samples=module.prepared_samples(value);self.assertEqual(len(samples),12)
            self.assertEqual(len({x['expectedArtifact']['sha256'] for x in samples}),12)
    def test_foreign_api_url_and_reused_sample_id_are_rejected_before_start(self):
        module=self.load()
        with tempfile.TemporaryDirectory() as folder:
            native={'apiBase':'http://127.0.0.1:18083','project':'retainedaccept'+'a'*10,'sourceRevision':'b'*40}
            for changed in ('url','duplicate'):
                value=self.fixture(folder)
                if changed=='url':value['apiBase']='http://127.0.0.1:6767'
                else:value['samples'][1]['sampleId']=value['samples'][0]['sampleId']
                path=Path(folder)/('fixture-'+changed+'.json');path.write_text(json.dumps(value));path.chmod(0o600)
                with patch.object(module,'verify_native_fixture',return_value=native),self.assertRaises(ValueError):module.read_fixture(path,environments=4,iterations=3)
    def test_distinct_samples_cannot_claim_the_same_actual_run_identity(self):
        module=self.load()
        with tempfile.TemporaryDirectory() as folder:
            fixture=self.fixture(folder);records=[]
            for sample in module.prepared_samples(fixture):
                binding={'schema':'workspace.perf.run.sample.v1','sampleId':sample['sampleId'],'operationId':sample['operationId'],
                         'workspaceId':fixture['workspaceId'],'modelConfigRef':'model-owned','sessionId':'session-owned','agentRunId':'same-run'}
                records.extend([{**binding,'phase':'accepted'},{**binding,'phase':'verified','artifact':sample['expectedArtifact']}])
            with self.assertRaisesRegex(ValueError,'distinct.*Run'):
                module.validate_records(records,fixture)

    def test_client_tool_latency_requires_actual_finite_receive_time_for_every_verified_run(self):
        module=self.load()
        with tempfile.TemporaryDirectory() as folder:
            fixture=self.fixture(folder);records=[]
            for index,sample in enumerate(module.prepared_samples(fixture)):
                binding={'schema':'workspace.perf.run.sample.v1','sampleId':sample['sampleId'],'operationId':sample['operationId'],
                         'workspaceId':fixture['workspaceId'],'modelConfigRef':'model-owned','sessionId':'session-'+str(index),'agentRunId':'run-'+str(index)}
                records.extend([{**binding,'phase':'accepted'},{**binding,'phase':'verified','artifact':sample['expectedArtifact'],
                    'firstSuccessfulToolResultMs':index+0.5}])
            measured=module.validate_records(records,fixture)['firstSuccessfulToolResultLatency']
            self.assertEqual(measured['source'],'k6SseReceive')
            self.assertEqual(measured['valuesMs'],[index+0.5 for index in range(12)])
            for invalid in (True,-1,float('nan'),float('inf'),'100'):
                records[-1]['firstSuccessfulToolResultMs']=invalid
                with self.subTest(invalid=invalid),self.assertRaises(ValueError):module.validate_records(records,fixture)
            records[-1].pop('firstSuccessfulToolResultMs')
            self.assertEqual(module.validate_records(records,fixture)['firstSuccessfulToolResultLatency'],'notObserved')

    def test_console_record_accepts_only_the_json_log_protocol(self):
        module=self.load()
        self.assertIsNone(module.console_record('ordinary K6 progress'))
        record={'schema':'workspace.perf.run.sample.v1','phase':'accepted','sampleId':'a'*32}
        self.assertEqual(module.console_record(json.dumps({'msg':'PERF_RUN '+json.dumps(record)})),record)
        with self.assertRaises(ValueError):module.console_record('{broken PERF_RUN ')

    def test_external_fixture_supports_twenty_environments_without_bypassing_target_binding(self):
        module=self.load()
        with tempfile.TemporaryDirectory() as folder:
            value=self.fixture(folder)
            value['samples']=[{'sampleId':f'{i+1:032x}','operationId':f'{i+101:032x}','workUnits':2} for i in range(60)]
            path=Path(folder)/'twenty.json';path.write_text(json.dumps(value));path.chmod(0o600)
            native={'apiBase':'http://127.0.0.1:18083','project':'retainedaccept'+'a'*10,'sourceRevision':'b'*40}
            with patch.object(module,'verify_native_fixture',return_value=native):
                self.assertEqual(len(module.read_fixture(path,environments=20,iterations=3)['samples']),60)

    def test_initial_stagger_cannot_exceed_bounded_total_start_delay(self):
        module=self.load()
        with tempfile.TemporaryDirectory() as folder:
            fixture=self.fixture(folder)
            for value in (-1,120001,20000):
                with self.assertRaises(ValueError):module.k6_command(fixture,folder,'/k6',environments=8,iterations=3,initial_stagger_ms=value)

    def test_stream_deadline_is_bounded_independently_from_parent_timeout(self):
        module=self.load()
        with tempfile.TemporaryDirectory() as folder:
            fixture=self.fixture(folder)
            for value in (0,-1,601,1.5,True):
                with self.assertRaises(ValueError):module.k6_command(fixture,folder,'/k6',environments=4,iterations=3,run_terminal_timeout_seconds=value)
                with self.assertRaises(ValueError):module.run('/not-read',folder,'/k6',run_terminal_timeout_seconds=value)

    def test_k6_options_receive_the_same_explicit_budget_as_the_parent_process(self):
        module=self.load()
        with tempfile.TemporaryDirectory() as folder:
            _,environment=module.k6_command(self.fixture(folder),folder,'/k6',environments=4,iterations=3,
                between_run_ms=30000,run_terminal_timeout_seconds=300)
            self.assertEqual(json.loads(environment['PERF_K6_BUDGET']),
                {'scenarioSeconds':1230,'setupSeconds':180,'gracefulStopSeconds':30})

    def test_python_budget_drives_actual_javascript_scenario_options(self):
        import subprocess
        module=self.load()
        code = r"""const fs=require('node:fs'),vm=require('node:vm');
const environment=JSON.parse(process.argv[2]);
const count=Number(environment.PERF_ENVIRONMENT_COUNT)*Number(environment.PERF_RUNS_PER_ENVIRONMENT);
const samples=Array.from({length:count},(_,i)=>({sampleId:'sample-'+i,operationId:'operation-'+i}));
class Metric {add(){}}
const context=vm.createContext({__ENV:environment,open:()=>JSON.stringify(samples),Trend:Metric,Rate:Metric,Counter:Metric});
const source=fs.readFileSync(process.argv[1],'utf8').replace(/^import .*;\r?\n/gm,'').replace('export default function (data)','function workload(data)').replace(/export /g,'');
vm.runInContext(source+';globalThis.result=options;',context);
process.stdout.write(JSON.stringify(context.result));"""
        with tempfile.TemporaryDirectory() as folder:
            for environments,stagger in [(4,0),(8,3000)]:
                _,environment=module.k6_command(self.fixture(folder),folder,'/k6',environments=environments,iterations=3,
                    between_run_ms=30000,initial_stagger_ms=stagger,run_terminal_timeout_seconds=300)
                safe={key:value for key,value in environment.items() if key in
                    {'PERF_K6_BUDGET','PERF_ENVIRONMENT_COUNT','PERF_RUNS_PER_ENVIRONMENT','PERF_RUN_TERMINAL_TIMEOUT_SECONDS',
                     'PERF_INITIAL_STAGGER_MS','PERF_BETWEEN_RUN_MS'}}
                result=subprocess.run(['node','-e',code,str(SOURCE.parents[1]/'k6/scenarios/multi-environment-workload.js'),json.dumps(safe)],
                    capture_output=True,text=True,check=True,timeout=10)
                options=json.loads(result.stdout)
                scenario=options['scenarios']['environmentWork']
                self.assertEqual((scenario['vus'],scenario['iterations']),(environments,3))
                self.assertEqual(options['setupTimeout'],'180s')
                self.assertEqual(scenario['gracefulStop'],'30s')
                self.assertEqual(scenario['maxDuration'],str(1230+7*stagger//1000)+'s')

    def test_undersized_or_excessive_process_budget_is_rejected_before_fixture_access(self):
        module=self.load()
        for timeout in (900,1466,14401):
            with self.subTest(timeout=timeout),self.assertRaisesRegex(ValueError,'process.*budget'):
                module.run('/not-read','/not-created','/not-run',environments=4,iterations=3,
                    between_run_ms=30000,run_terminal_timeout_seconds=300,timeout=timeout)

if __name__=='__main__':unittest.main()
