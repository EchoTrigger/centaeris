"""Pure strict byte/binding contract for synchronous SDK maintenance results."""
import base64
import copy
import hashlib
from pathlib import Path
import json
from unittest import TestCase
from .provider_environment_maintenance_completion import validate_direct_completion, dispatch_record, SCHEMA, DISPATCH_SCHEMA

class ProviderDirectCompletionTests(TestCase):
    def receipt(self):
        fixture=Path(__file__).resolve().parents[3]/'tests/workspace/fixtures/provider_environment_maintenance_authorization/v1/valid.json'
        auth=json.loads(fixture.read_text())['payload']
        expected={key:auth[key] for key in ('environmentId','providerTarget','profileDigest','requestDigest','controllerEpoch','controllerLeaseDigest')}
        expected['operationId']=auth['controllerOwner']['operationId']
        expected['authorizationDigest']='sha256:'+hashlib.sha256(json.dumps(auth,sort_keys=True,separators=(',',':')).encode()).hexdigest()
        out=b'\x00\xffexact bytes'
        return {'schema':SCHEMA,**expected,'output':{'kind':'command','exitCode':7,
            'stdoutBytesBase64':base64.b64encode(out).decode(),'stdoutByteLength':len(out),
            'stderrBytesBase64':'','stderrByteLength':0}},expected

    def test_complete_binary_output_and_dispatch_record_are_exactly_bound(self):
        value,expected=self.receipt()
        validate_direct_completion(value,expected)
        self.assertEqual(dispatch_record(value),{'schema':DISPATCH_SCHEMA,**expected})
        for key in expected:
            changed=copy.deepcopy(expected)
            changed[key]=3 if key=='controllerEpoch' else 'different'
            with self.subTest(key=key),self.assertRaises(ValueError):
                validate_direct_completion(value,changed)
        self.assertNotIn('cleanupComplete',value)
        self.assertNotIn('taskIds',value)

    def test_missing_changed_truncated_or_oversized_output_is_rejected(self):
        value,expected=self.receipt()
        outputs=[dict(value['output'],stdoutByteLength=999),dict(value['output'],stdoutBytesBase64='bad@@'),
            dict(value['output'],exitCode=True),dict(value['output'],cleanupComplete=True),
            {'kind':'operationEnded','taskIds':[],'cleanupComplete':True},
            dict(value['output'],stdoutBytesBase64=base64.b64encode(b'x'*1024*1024).decode(),stdoutByteLength=1024*1024)]
        for output in outputs:
            with self.subTest(output=output['kind']),self.assertRaises(ValueError):
                validate_direct_completion(dict(value,output=output),expected)
        for key in value:
            missing=dict(value);missing.pop(key)
            with self.subTest(missing=key),self.assertRaises(ValueError):
                validate_direct_completion(missing,expected)

    def test_native_file_and_directory_results_keep_their_own_semantics(self):
        value,expected=self.receipt()
        data=b'\x00\xfffiles'
        for output in ({'kind':'fileWritten','byteLength':len(data),'sha256':'sha256:'+hashlib.sha256(data).hexdigest()},
                {'kind':'fileRead','byteLength':len(data),'sha256':'sha256:'+hashlib.sha256(data).hexdigest(),
                 'dataBase64':base64.b64encode(data).decode()},
                {'kind':'directoryListed','entries':[{'path':'/logs/verifier/reward.txt','kind':'file','sizeBytes':1}]}):
            validate_direct_completion(dict(value,output=output),expected)
        for output in ({'kind':'fileRead','byteLength':1,'sha256':'sha256:'+'a'*64,'dataBase64':'YQ=='},
                {'kind':'directoryListed','entries':[{'path':'/logs/link','kind':'symlink','sizeBytes':0}]},
                {'kind':'directoryListed','entries':[{'path':'/logs/../secret','kind':'file','sizeBytes':0}]}):
            with self.assertRaises(ValueError):
                validate_direct_completion(dict(value,output=output),expected)

    def test_confirmed_file_not_found_is_a_strict_failure_result(self):
        value, expected = self.receipt()
        fixture = Path(__file__).resolve().parents[3] / 'tests/workspace/fixtures/provider_environment_maintenance_completion/v1/file-not-found.json'
        failure = json.loads(fixture.read_text())
        validate_direct_completion(value | {'output': failure['output']}, expected)
        for output in ({'kind': 'filesystemFailed', 'code': 'unknown'},
                       {'kind': 'filesystemFailed', 'code': 'permissionDenied'},
                       {'kind': 'filesystemFailed', 'code': 'fileNotFound', 'message': 'private provider detail'},
                       {'kind': 'filesystemFailed'},
                       {'kind': 'filesystemFailed', 'code': ['fileNotFound']}):
            with self.subTest(output=output), self.assertRaises(ValueError):
                validate_direct_completion(value | {'output': output}, expected)
