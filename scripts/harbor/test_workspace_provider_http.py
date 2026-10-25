"""Typed provider HTTP consumption uses native SDK operation receipts."""
import base64
import hashlib
import json
from pathlib import Path
import socket
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from workspace_provisioner import WorkspaceHTTPProvisioner
from workspace_client import WorkspaceError
from workspace_provider import ProviderTarget,OwnedProviderEnvironment

class ProviderHTTPTests(unittest.TestCase):
    def setUp(self):
        self.profile={'schema':'runtime.e2b_environment_profile.v1','provider':'e2b','providerConfigDigest':'sha256:'+'f'*64,
            'templateRef':{'templateId':'task-template','buildId':'123e4567-e89b-12d3-a456-426614174000'},
            'workspaceRoot':'/app','user':'root','network':'publicInternet','lifetime':'retained',
            'timeoutSeconds':3600,'inputBudgetBytes':67108864,'workspaceInitialization':'taskImage'}
        self.target=ProviderTarget('e2b','task-create','instance_'+'a'*64,'sandbox-task','sha256:'+'b'*64,'sha256:'+'f'*64).to_wire()
        self.profile_digest=WorkspaceHTTPProvisioner._digest(self.profile)
        self.requests=[];self.files={};self.completions={};self.dispatches=0
        self.lose_reply=False;self.corrupt_result=False;self.state='resident';self.epoch=2
        self.output_override=None;self.unknown_reply=False;self.reject_release=False
        self.reply_errors=[]
        owner=self
        class Handler(BaseHTTPRequestHandler):
            def log_message(self,*args):pass
            def do_POST(self):
                raw=self.rfile.read(int(self.headers['Content-Length']))
                content=b''
                if self.path.endswith('/filesystem'):
                    count=int.from_bytes(raw[:4],'big');value=json.loads(raw[4:4+count]);content=raw[4+count:]
                else:value=json.loads(raw)
                owner.requests.append((self.path,value,content))
                if self.path.endswith('/environment-profile'):
                    result={'schema':'workspace.environment_profile.bound.v1','operationId':value['operationId'],
                        'sessionId':'session','environmentId':'environment','profileDigest':owner.profile_digest,'profile':owner.profile}
                elif self.path.endswith('/observe'):
                    result={'schema':'runtime.provider_environment.observe.result.v1','sessionId':'session','environmentId':'environment',
                        'profile':owner.profile,'profileDigest':owner.profile_digest,'state':owner.state,'controllerEpoch':owner.epoch,
                        'controllerOwner':None,'providerTarget':None if owner.state=='unallocated' else owner.target,'templateRef':owner.profile['templateRef'],
                        'resourceFacts':{'cpuCount':1,'memoryMiB':2048} if owner.state=='resident' else None,
                        'resourceHolding':0 if owner.state in {'destroyed','unallocated'} else 1}
                elif self.path.endswith('/provision-authorizations'):
                    owner.epoch+=1
                    logical={'kind':'provisionEnvironment','environmentProfile':owner.profile,'environmentProfileDigest':owner.profile_digest,
                        'userId':'owner','agentId':'agent','pluginActivation':{'schema':'runtime.plugin_activation.v1','packages':[]}}
                    payload={'schema':'workspace.environment_provision_authorization.v1','operationKind':'provision',
                        'workspaceId':'workspace','sessionId':'session','environmentId':'environment',
                        'environmentProfile':owner.profile,'environmentProfileDigest':owner.profile_digest,
                        'userId':'owner','agentId':'agent','pluginActivation':logical['pluginActivation'],
                        'controllerOwner':{'kind':'maintenanceOperation','operationId':value['operationId']},
                        'controllerEpoch':owner.epoch,'controllerPurpose':'execute','controllerLeaseDigest':'sha256:'+'c'*64,
                        'expiresAtMs':4000000000000,'requestDigest':WorkspaceHTTPProvisioner._digest(logical)}
                    result={'payload':payload,'signature':'hmac-sha256:'+'d'*64}
                elif self.path.endswith('/provision'):
                    owner.state='resident'
                    result={'schema':'runtime.provider_environment.provision.result.v1','status':'completed',
                        'operationId':value['authorization']['payload']['controllerOwner']['operationId'],'environmentId':'environment',
                        'creationAttemptId':owner.target['creationAttemptId'],'instanceId':owner.target['instanceId'],
                        'sandboxId':owner.target['sandboxId'],'profileDigest':owner.profile_digest,'preparationDigest':'sha256:'+'e'*64}
                elif self.path.endswith('/maintenance-authorizations'):
                    owner.epoch+=1
                    payload={'schema':'workspace.provider_environment_maintenance_authorization.v1','operationKind':'task',
                        'workspaceId':'workspace','sessionId':'session','environmentId':'environment','providerTarget':owner.target,
                        'profileDigest':owner.profile_digest,'controllerOwner':{'kind':'maintenanceOperation','operationId':value['operationId']},
                        'controllerEpoch':owner.epoch,'controllerPurpose':'execute','controllerLeaseDigest':'sha256:'+'c'*64,
                        'expiresAtMs':4000000000000,'requestDigest':WorkspaceHTTPProvisioner._digest(value['request'])}
                    result={'payload':payload,'signature':'hmac-sha256:'+'d'*64}
                elif self.path.endswith('/maintenance/task') or self.path.endswith('/filesystem'):
                    if owner.unknown_reply:
                        self.connection.shutdown(socket.SHUT_RDWR);self.connection.close();return
                    payload=value['authorization']['payload'];operation=payload['controllerOwner']['operationId']
                    result=owner.completions.get(operation)
                    if result is None:
                        owner.dispatches+=1
                        logical=value['request']
                        if 'program' in logical:
                            output={'kind':'command','exitCode':0,'stdoutBytesBase64':base64.b64encode(b'native command').decode(),
                                'stdoutByteLength':14,'stderrBytesBase64':'','stderrByteLength':0}
                        else:
                            native=logical['request'];path=native['path'];kind=native['kind']
                            if kind=='writeFile':
                                if len(content)!=logical['contentLength'] or 'sha256:'+hashlib.sha256(content).hexdigest()!=logical['contentDigest']:
                                    self.send_error(400);return
                                owner.files[path]=content
                                output={'kind':'fileWritten','byteLength':len(content),'sha256':logical['contentDigest']}
                            elif kind=='readFile':
                                data=owner.files[path]
                                output={'kind':'fileRead','dataBase64':base64.b64encode(data).decode(),'byteLength':len(data),
                                    'sha256':'sha256:'+hashlib.sha256(data).hexdigest()}
                            else:
                                output={'kind':'directoryListed','entries':[{'path':name,'kind':'file','sizeBytes':len(data)}
                                    for name,data in owner.files.items() if name.startswith(path+'/')]}
                        if owner.output_override is not None:output=owner.output_override
                        result={'schema':'runtime.provider_environment.maintenance.completed.v1','operationId':operation,
                            **{name:payload[name] for name in ('environmentId','providerTarget','profileDigest','requestDigest','controllerEpoch','controllerLeaseDigest')},
                            'authorizationDigest':WorkspaceHTTPProvisioner._digest(payload),'output':output}
                        owner.completions[operation]=result
                    if owner.reply_errors:
                        status,code=owner.reply_errors.pop(0)
                        body=json.dumps({'error':code}).encode()
                        self.send_response(status);self.send_header('Content-Type','application/json')
                        self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body);return
                    if owner.lose_reply:
                        owner.lose_reply=False;self.connection.shutdown(socket.SHUT_RDWR);self.connection.close();return
                    if owner.corrupt_result:result=result|{'requestDigest':'sha256:'+'e'*64}
                elif self.path.endswith('/release'):
                    if owner.reject_release:self.send_error(503);return
                    operation=self.path.split('/')[-2]
                    result={'environmentId':'environment','controllerOwner':{'kind':'maintenanceOperation','operationId':operation},
                        'controllerEpoch':value['controllerEpoch'],'controllerLeaseDigest':value['controllerLeaseDigest'],'released':True}
                elif self.path.endswith('/retire'):
                    owner.state='destroyed'
                    result={'schema':'runtime.provider_environment.retire.result.v1','operationId':value['operationId'],
                        'environmentId':'environment','creationAttemptId':owner.target['creationAttemptId'],
                        'sandboxId':owner.target['sandboxId'],'status':'removed'}
                else:self.send_error(404);return
                body=json.dumps(result).encode();self.send_response(200);self.send_header('Content-Type','application/json')
                self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
        server=ThreadingHTTPServer(('127.0.0.1',0),Handler)
        threading.Thread(target=server.serve_forever,daemon=True).start()
        self.addCleanup(server.server_close);self.addCleanup(server.shutdown)
        origin=f'http://127.0.0.1:{server.server_port}'
        self.provisioner=WorkspaceHTTPProvisioner(api_origin=origin,runtime_origin=origin,
            maintenance_token='fake-maintenance-'+'m'*32,internal_token='fake-internal-'+'i'*32)
        self.provisioner.bind_profile('session','environment',self.profile,'bind-profile')

    def test_deadline_and_busy_replies_recover_one_durable_operation_before_release(self):
        self.reply_errors=[(504,'request_deadline_exceeded'),(409,'provider_maintenance_environment_busy')]
        owned=self.provisioner.observe_setup('session','environment','run')
        output=self.provisioner.exec(owned,command='verify',cwd='/app',timeout_sec=5)
        self.assertEqual(output['stdout'],'native command')
        requests=[value for path,value,_ in self.requests if path.endswith('/maintenance/task')]
        self.assertEqual(len(requests),3)
        self.assertTrue(all(value==requests[0] for value in requests))
        self.assertEqual(self.dispatches,1)
        self.assertEqual(sum(path.endswith('/maintenance-authorizations') for path,_,_ in self.requests),1)
        self.assertEqual(sum(path.endswith('/release') for path,_,_ in self.requests),1)

    def test_rejected_maintenance_authority_is_not_retried_or_released(self):
        self.reply_errors=[(409,'provider_maintenance_binding_rejected')]
        owned=self.provisioner.observe_setup('session','environment','run')
        with self.assertRaisesRegex(WorkspaceError,'provider_maintenance_binding_rejected'):
            self.provisioner.exec(owned,command='verify',cwd='/app',timeout_sec=5)
        self.assertEqual(sum(path.endswith('/maintenance/task') for path,_,_ in self.requests),1)
        self.assertFalse(any(path.endswith('/release') for path,_,_ in self.requests))

    def test_unknown_operation_recovery_is_bounded_and_never_releases(self):
        from unittest.mock import patch
        self.reply_errors=[(503,'provider_maintenance_outcome_unknown')]*10
        owned=self.provisioner.observe_setup('session','environment','run')
        with patch('workspace_provisioner.time.monotonic',side_effect=[0,1,20]), patch('workspace_provisioner.time.sleep'):
            with self.assertRaisesRegex(WorkspaceError,'provider_maintenance_outcome_unknown'):
                self.provisioner.exec(owned,command='verify',cwd='/app',timeout_sec=5)
        self.assertEqual(self.dispatches,1)
        self.assertFalse(any(path.endswith('/release') for path,_,_ in self.requests))

    def test_provider_observe_exec_and_retire_never_fake_native_identity_or_task_protocol(self):
        owned=self.provisioner.observe_setup('session','environment','run')
        self.assertIsInstance(owned,OwnedProviderEnvironment)
        self.assertEqual(owned.provider_target.to_wire(),self.target)
        self.lose_reply=True
        output=self.provisioner.exec(owned,command='printf native',cwd='/app',env={'FLAG':'kept'},user='root',timeout_sec=5)
        self.assertEqual(output,{'returnCode':0,'stdout':'native command','stderr':''})
        requests=[value for path,value,_ in self.requests if path.endswith('/maintenance/task')]
        self.assertEqual(len(requests),2);self.assertEqual(requests[0],requests[1]);self.assertEqual(self.dispatches,1)
        self.assertEqual(set(requests[0]),{'authorization','request'})
        self.assertEqual(requests[0]['request'],{'program':'/usr/bin/env','args':['--','FLAG=kept','/bin/bash','-lc','printf native'],
            'cwd':'/app','timeoutMs':5000})
        self.assertEqual(self.provisioner.retire(owned,'retire-op'),'destroyed')
        retirement=next(value for path,value,_ in self.requests if path.endswith('/retire'))
        self.assertEqual(set(retirement),{'schema','operationId','environmentId','creationAttemptId','sandboxId','providerConfigDigest','expectedControllerEpoch','acknowledgeDataLoss'})
        observed=self.provisioner.observe_cleanup(owned)
        self.assertEqual((observed.physical_state,observed.resource_holding),('absent',0))

    def test_provider_native_file_ports_preserve_binary_data_and_safe_directory_extraction(self):
        owned=self.provisioner.observe_setup('session','environment')
        with tempfile.TemporaryDirectory() as directory:
            source=Path(directory)/'source';source.write_bytes(b'\0\xffbinary files')
            self.provisioner.upload_file(owned,source,'/logs/verifier/reward.txt')
            target=Path(directory)/'download'
            self.provisioner.download_dir(owned,'/logs/verifier',target)
            self.assertEqual((target/'reward.txt').read_bytes(),source.read_bytes())
        files=[value['request']['request'] for path,value,_ in self.requests if path.endswith('/filesystem')]
        self.assertEqual([request['kind'] for request in files],['writeFile','listDirectory','readFile'])
        self.assertTrue(all('policy' not in request and 'createOnly' not in request for request in files))

    def test_changed_direct_completion_is_not_released_or_reported_as_success(self):
        owned=self.provisioner.observe_setup('session','environment')
        self.corrupt_result=True
        with self.assertRaises(WorkspaceError):self.provisioner.exec(owned,command='verifier',timeout_sec=5)
        self.assertFalse(any(path.endswith('/release') for path,_,_ in self.requests))

    def test_provider_provision_uses_historical_receipt_then_fresh_same_target_observation(self):
        self.state='unallocated'
        owned=self.provisioner.provision('session','environment','provision-op')
        self.assertIsInstance(owned,OwnedProviderEnvironment)
        self.assertEqual(owned.provider_target.to_wire(),self.target)
        self.assertEqual(owned.template_id,self.profile['templateRef']['templateId'])
        calls=[path for path,_,_ in self.requests if path.endswith(('/observe','/provision','/release'))]
        self.assertEqual(calls,['/internal/environments/observe','/internal/environments/provision',
            '/internal/environments/environment/maintenance-operations/provision-op/release','/internal/environments/observe'])

    def test_confirmed_missing_directory_releases_before_reporting_failure_and_can_retire(self):
        owned = self.provisioner.observe_setup('session', 'environment')
        self.output_override = {'kind': 'filesystemFailed', 'code': 'fileNotFound'}
        self.lose_reply = True
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(WorkspaceError, 'maintenance_file_not_found'):
                self.provisioner.download_dir(owned, '/logs/missing', Path(directory) / 'artifacts')
        calls = [path for path, _, _ in self.requests if path.endswith(('/filesystem', '/release'))]
        self.assertEqual(len(calls), 3)
        self.assertTrue(calls[-1].endswith('/release'))
        self.assertEqual(self.dispatches, 1)
        self.assertEqual(self.provisioner.retire(owned, 'retire-after-failure'), 'destroyed')
        self.assertEqual(self.provisioner.observe_cleanup(owned).resource_holding, 0)

    def test_confirmed_missing_file_releases_and_preserves_existing_local_destination(self):
        owned = self.provisioner.observe_setup('session', 'environment')
        self.files['/logs/result'] = b'fixture provider payload'
        self.output_override = {'kind': 'filesystemFailed', 'code': 'fileNotFound'}
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / 'result'
            target.write_bytes(b'local original')
            with self.assertRaisesRegex(WorkspaceError, 'maintenance_file_not_found'):
                self.provisioner.download_file(owned, '/logs/result', target)
            self.assertEqual(target.read_bytes(), b'local original')
            self.assertEqual(list(Path(directory).iterdir()), [target])
        self.assertTrue(any(path.endswith('/release') for path, _, _ in self.requests))

    def test_malformed_or_changed_failure_receipt_never_releases(self):
        owned = self.provisioner.observe_setup('session', 'environment')
        for output, corrupt in (({'kind': 'filesystemFailed', 'code': 'unknown'}, False),
                                ({'kind': 'filesystemFailed', 'code': 'fileNotFound', 'message': 'private detail'}, False),
                                ({'kind': 'filesystemFailed', 'code': 'fileNotFound'}, True)):
            with self.subTest(output=output, corrupt=corrupt):
                self.requests.clear()
                self.output_override, self.corrupt_result = output, corrupt
                with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(WorkspaceError, 'provider_maintenance_output_invalid'):
                    self.provisioner.download_dir(owned, '/logs/missing', Path(directory) / 'artifacts')
                self.assertFalse(any(path.endswith('/release') for path, _, _ in self.requests))

    def test_file_failure_cannot_complete_command_or_write_requests(self):
        owned = self.provisioner.observe_setup('session', 'environment')
        self.output_override = {'kind': 'filesystemFailed', 'code': 'fileNotFound'}
        with self.assertRaisesRegex(WorkspaceError, 'provider_maintenance_output_invalid'):
            self.provisioner.exec(owned, command='printf hello')
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / 'source'; source.write_bytes(b'write')
            with self.assertRaisesRegex(WorkspaceError, 'provider_maintenance_output_invalid'):
                self.provisioner.upload_file(owned, source, '/app/file')
        self.assertFalse(any(path.endswith('/release') for path, _, _ in self.requests))

    def test_unknown_file_outcome_keeps_maintenance_responsibility(self):
        owned = self.provisioner.observe_setup('session', 'environment')
        self.unknown_reply = True
        with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(WorkspaceError, 'transport_unavailable'):
            self.provisioner.download_dir(owned, '/logs/missing', Path(directory) / 'artifacts')
        self.assertEqual(sum(path.endswith('/filesystem') for path, _, _ in self.requests), 2)
        self.assertFalse(any(path.endswith('/release') for path, _, _ in self.requests))

    def test_failed_release_is_not_hidden_by_a_file_not_found_result(self):
        owned = self.provisioner.observe_setup('session', 'environment')
        self.output_override = {'kind': 'filesystemFailed', 'code': 'fileNotFound'}
        self.reject_release = True
        with tempfile.TemporaryDirectory() as directory, self.assertRaises(WorkspaceError) as raised:
            self.provisioner.download_dir(owned, '/logs/missing', Path(directory) / 'artifacts')
        self.assertNotEqual(raised.exception.code, 'maintenance_file_not_found')
        self.assertTrue(any(path.endswith('/release') for path, _, _ in self.requests))
