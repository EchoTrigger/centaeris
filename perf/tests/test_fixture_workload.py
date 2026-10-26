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

if __name__=='__main__':unittest.main()
