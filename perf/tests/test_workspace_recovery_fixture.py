"""Real fixed commands and the existing k6 smoke boundary; no product services."""
import importlib.util
import io
import json
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def load(path):
    spec = importlib.util.spec_from_file_location('recovery_fixture_subject', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class RecoveryFixtureTests(unittest.TestCase):
    def smoke(self, fixture):
        code = r"""const fs=require('node:fs'),vm=require('node:vm');
const fixture=JSON.parse(process.argv[2]),calls=[];
let source=fs.readFileSync(process.argv[1],'utf8').replace(/^import .*;\r?$/gm,'').replace(/export const /g,'const ').replace('export default function ()','function gate()');
class Metric { add(){} }
const context={__ENV:fixture===null?{}:{PERF_RUN_FIXTURE_PATH:'/owned/phase.json'},open:()=>{if(context.started)throw new Error('k6 only permits open during initialization');return JSON.stringify(fixture)},
Trend:Metric,Rate:Metric,check:()=>true,console:{log:(v)=>calls.push(['log',v])},
configuredTarget:()=>({workspaceId:'workspace',agentId:'agent',modelId:'model'}),login:()=>({store:{},csrf:'csrf'}),
startRun:()=>{calls.push(['admit']);return {sessionId:'new-session',agentRunId:'new-run'}},
artifactExpectation:()=>({filename:'default.bin',sizeBytes:1,sha256:'sha256:'+'a'.repeat(64)}),
waitForTerminal:(store,csrf,s,r)=>{calls.push(['observe',s,r]);return {status:'completed',events:[]}},
verifyToolRun:()=>{},verifyFixedCommand:(result,digest)=>calls.push(['command',digest]),
verifyPublishedArtifact:(store,result,s,r,expected)=>{calls.push(['artifact',s,r,expected]);return {downloadMs:1,...expected}}};
try {vm.runInNewContext(source+'\nglobalThis.started=true;gate();',context);process.stdout.write(JSON.stringify({calls}));}
catch(e){process.stdout.write(JSON.stringify({calls,error:e.message}));}
"""
        result = subprocess.run(['node', '-e', code, str(ROOT / 'perf/k6/scenarios/run-smoke.js'), json.dumps(fixture)],
                                capture_output=True, text=True, timeout=10, check=True)
        return json.loads(result.stdout)

    def test_k6_observes_explicit_admitted_run_and_preserves_default_admission(self):
        fixture = {'schema': 'workspace.perf.run.observation.v1', 'sessionId': 'session-a', 'agentRunId': 'run-a',
                   'expectedCommandSha256': 'sha256:' + 'b' * 64,
                   'expectedArtifact': {'filename': 'recovery.bin', 'sizeBytes': 32, 'sha256': 'sha256:' + 'c' * 64}}
        result = self.smoke(fixture)
        self.assertNotIn('error', result)
        self.assertNotIn(['admit'], result['calls'])
        self.assertIn(['observe', 'session-a', 'run-a'], result['calls'])
        self.assertIn(['command', fixture['expectedCommandSha256']], result['calls'])
        self.assertIn(['artifact', 'session-a', 'run-a', fixture['expectedArtifact']], result['calls'])
        self.assertIn(['admit'], self.smoke(None)['calls'])
        for bad in ({**fixture, 'extra': True}, {**fixture, 'sessionId': '../other'},
                    {**fixture, 'expectedCommandSha256': 'invalid'},
                    {**fixture, 'expectedArtifact': {**fixture['expectedArtifact'], 'sizeBytes': -1}}):
            result = self.smoke(bad)
            self.assertIn('error', result)
            self.assertFalse(any(call[0] in ('admit', 'observe') for call in result['calls']))

    def test_fixed_commands_restore_original_bytes_without_recreating_missing_or_corrupt_files(self):
        module = load(ROOT / 'perf/mock-model/fixed_workload.py')
        case = 'a' * 32
        with tempfile.TemporaryDirectory() as directory:
            def execute(phase):
                sample = {'schema': 'workspace.recovery.fixture.v1', 'caseId': case, 'phase': phase}
                argv = shlex.split(module.recovery_command(sample, output_directory=directory))
                argv[0] = sys.executable
                return subprocess.run(argv, capture_output=True, timeout=10)
            self.assertNotEqual(execute('read').returncode, 0)
            self.assertEqual(list(Path(directory).iterdir()), [])
            self.assertEqual(execute('write').returncode, 0)
            path = Path(directory) / module.recovery_expectation(case)['filename']
            original = path.read_bytes()
            self.assertGreater(len(original), 0)
            self.assertEqual(execute('readAndAdvance').returncode, 0)
            self.assertEqual(execute('read').returncode, 0)
            self.assertEqual(path.read_bytes(), original)
            path.write_bytes(b'corrupted')
            self.assertNotEqual(execute('readAndAdvance').returncode, 0)
            self.assertEqual(path.read_bytes(), b'corrupted')

    def test_protocol_rejects_unknown_fields_aliases_and_arbitrary_commands(self):
        module = load(ROOT / 'perf/mock-model/fixed_workload.py')
        sample = {'schema': 'workspace.recovery.fixture.v1', 'caseId': 'a' * 32, 'phase': 'write'}
        self.assertEqual(module.recovery_sample('PERF_WORKSPACE_RECOVERY ' + json.dumps(sample)), sample)
        for changed in ({**sample, 'command': 'anything'}, {**sample, 'phase': 'restore'},
                        {**sample, 'caseId': '../foreign'}, {**sample, 'schema': 'old'}):
            with self.subTest(sample=changed), self.assertRaises(ValueError):
                module.recovery_sample('PERF_WORKSPACE_RECOVERY ' + json.dumps(changed))

    def test_actual_model_emits_fixed_command_then_exact_artifact_then_final(self):
        module = load(ROOT / 'perf/mock-model/server.py')
        sample = {'schema': 'workspace.recovery.fixture.v1', 'caseId': 'b' * 32, 'phase': 'read'}
        outputs = []
        for rounds in range(3):
            handler = object.__new__(module.Handler)
            handler.path = '/v1/responses'
            value = {'model': 'perf-workspace-recovery', 'stream': True, 'input': [
                {'role': 'user', 'content': 'PERF_WORKSPACE_RECOVERY ' + json.dumps(sample)},
                *[{'type': 'function_call_output', 'call_id': f'call-{i}', 'output': 'result'} for i in range(rounds)]]}
            body = json.dumps(value).encode()
            handler.headers = {'content-length': str(len(body))}
            handler.rfile = io.BytesIO(body)
            handler.wfile = io.BytesIO()
            handler.send_response = lambda *args: None
            handler.send_header = lambda *args: None
            handler.end_headers = lambda: None
            handler.do_POST()
            events = [json.loads(line[6:]) for line in handler.wfile.getvalue().decode().splitlines() if line.startswith('data: ')]
            outputs.append(events[-1]['response']['output'][0])
        self.assertEqual(outputs[0]['name'], 'bash')
        self.assertEqual(json.loads(outputs[0]['arguments'])['command'], module._fixed_workload.recovery_command(sample))
        self.assertEqual(outputs[1]['name'], 'publish_artifact')
        self.assertEqual(json.loads(outputs[1]['arguments']), {'path': '/mnt/data/recovery-' + 'b' * 32 + '.bin'})
        self.assertEqual(outputs[2]['type'], 'message')


if __name__ == '__main__':
    unittest.main()
