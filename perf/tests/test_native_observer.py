import copy
import importlib.util
from pathlib import Path
import unittest

SOURCE=Path(__file__).resolve().parents[1]/'harness/native_observer.py'

class NativeObservationTests(unittest.TestCase):
    def subject(self):
        spec=importlib.util.spec_from_file_location('native_observer_subject',SOURCE)
        module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module
    def sample(self, tick):
        containers=[]
        for index in range(4):
            target={'containerId':str(index)*64,'environmentId':f'env-{index}', 'environmentInstanceId':f'instance-{index}', 'profileDigest':'sha256:'+'a'*64,'imageDigest':'sha256:'+'b'*64}
            containers.append({'state':'observed','resources':{'target':target,'initPid':100+index,'initStartTicks':500+index,
                'cgroupIdentity':{'path':f'/sys/fs/cgroup/test/{index}','device':1,'inode':1000+index},
                'cpuCounters':{'usageUsec':tick*150000}, 'memoryCurrentBytes':100000000}})
        return {'schema':'workspace.perf.native_sample.v1','host':{'schema':'native.host.resources.v1','hostId':'private-host','bootId':'a'*36,
            'observedAtBootMs':1000+tick*1000,'collectionDurationMs':5,'containers':containers},
            'states':{str(i)*64:{'running':True,'paused':False,'inspection':{'schema':'environment.task.store.inspection.v2','instanceId':f'instance-{i}','activeTasks':1},'caps':{'memoryBytes':4*1024**3,'cpuMilli':2000,'pids':512}} for i in range(4)}}
    def test_real_cpu_and_task_overlap_requires_two_consecutive_intervals(self):
        module=self.subject();samples=[self.sample(i) for i in range(3)]
        report=module.summarize(samples,4)
        self.assertEqual(report['physicalOverlap'],'verified');self.assertEqual(report['peakWorkingEnvironments'],4)
        self.assertEqual(report['consecutiveWorkingIntervals'],2)
        self.assertEqual(module.summarize(samples[:2],4)['physicalOverlap'],'notObserved')
    def test_unknown_identity_change_zero_cpu_or_no_active_task_cannot_count_as_four(self):
        module=self.subject()
        for defect in ('unknown','identity','cpu','active','paused','duplicate','caps'):
            samples=[self.sample(i) for i in range(3)]
            for sample in samples[1:]:
                item=sample['host']['containers'][3]
                if defect=='unknown': sample['host']['containers'][3]={'state':'unknown','target':item['resources']['target'],'reason':'probe_failed'}
                elif defect=='identity': item['resources']['cgroupIdentity']['inode']+=sample['host']['observedAtBootMs']
                elif defect=='cpu': item['resources']['cpuCounters']['usageUsec']=0
                elif defect=='active': sample['states']['3'*64]['inspection']['activeTasks']=0
                elif defect=='paused': sample['states']['3'*64]['paused']=True
                elif defect=='caps': sample['states']['3'*64]['caps']['memoryBytes']=1024**3
                else: item['resources']['target']['environmentId']='env-0'
            report=module.summarize(samples,4)
            self.assertEqual(report['physicalOverlap'],'notObserved',defect)
            self.assertLess(report['peakWorkingEnvironments'],4,defect)
    def test_host_boot_change_breaks_continuity_and_unknown_is_retained(self):
        module=self.subject();samples=[self.sample(i) for i in range(3)];samples[1]['host']['bootId']='changed'
        samples.append({'schema':'workspace.perf.native_sample.v1','state':'unknown','reason':'resource_probe_failed'})
        report=module.summarize(samples,4)
        self.assertEqual(report['physicalOverlap'],'notObserved');self.assertIn('resource_probe_failed',report['unknownReasons'])

    def test_sparse_gate_distinguishes_inventory_from_observed_working_target(self):
        module=self.subject();samples=[self.sample(i) for i in range(3)]
        for sample in samples:
            for index in (2,3):sample['states'][str(index)*64]['inspection']['activeTasks']=0
        report=module.summarize(samples,4,expected_working_environments=2)
        self.assertEqual(report['physicalOverlap'],'verified')
        self.assertEqual(report['expectedEnvironments'],4)
        self.assertEqual(report['expectedWorkingEnvironments'],2)
        self.assertEqual(report['peakWorkingEnvironments'],2)
        for target in (0,5):
            with self.assertRaises(ValueError):module.NativeObserver('/manifest','/output',4,expected_working_environments=target)

class NativeCollectionWindowTests(unittest.TestCase):
    def collect(self, *, failure=None, wall_times=(1700000000.25, 1700000000.75)):
        import json
        import tempfile
        from unittest.mock import patch
        module = NativeObservationTests().subject()
        observer = module.NativeObserver('/manifest', '/output', 4)
        events = []
        times = iter(wall_times)
        def wall_time():
            value = next(times)
            events.append(('wall', value))
            return value
        def sample():
            events.append(('sample',))
            observer._stop.set()
            if failure is not None:
                raise failure
            return NativeObservationTests().sample(0)
        with tempfile.TemporaryDirectory() as directory:
            observer.output = Path(directory) / 'observations.jsonl'
            with patch.object(observer, '_sample', side_effect=sample) as probe, patch.object(module.time, 'time', side_effect=wall_time):
                observer._loop()
            probe.assert_called_once_with()
            recorded = [json.loads(line) for line in observer.output.read_text().splitlines()]
        self.assertEqual(recorded, observer.samples)
        return module, observer.samples[0], events

    def test_successful_collection_records_wall_times_around_the_actual_sample(self):
        module, sample, events = self.collect()
        self.assertEqual(sample['collectionWindow'], {'startedAtUnixMs': 1700000000250, 'completedAtUnixMs': 1700000000750})
        self.assertEqual(events, [('wall', 1700000000.25), ('sample',), ('wall', 1700000000.75)])
        self.assertIn('host', sample)
        samples = [NativeObservationTests().sample(i) for i in range(3)]
        expected = module.summarize(samples, 4)
        for item in samples:
            item['collectionWindow'] = sample['collectionWindow']
        self.assertEqual(module.summarize(samples, 4), expected)

    def test_unknown_collection_also_records_its_real_window_and_still_cannot_count_overlap(self):
        module, sample, events = self.collect(failure=TimeoutError('private probe error'))
        self.assertEqual(sample['collectionWindow'], {'startedAtUnixMs': 1700000000250, 'completedAtUnixMs': 1700000000750})
        self.assertEqual(events, [('wall', 1700000000.25), ('sample',), ('wall', 1700000000.75)])
        self.assertEqual(sample['state'], 'unknown')
        self.assertEqual(sample['reason'], 'resource_probe_timeouterror')
        report = module.summarize([sample, sample, sample], 4)
        self.assertEqual(report['physicalOverlap'], 'notObserved')
        self.assertEqual(report['peakWorkingEnvironments'], 0)
        self.assertEqual(report['consecutiveWorkingIntervals'], 0)
        self.assertEqual(report['unknownReasons'], ['resource_probe_timeouterror'])

    def test_wall_clock_rollback_is_retained_without_fabricating_a_monotonic_window(self):
        for failure in (None, TimeoutError('probe timeout')):
            with self.subTest(unknown=failure is not None):
                _, sample, _ = self.collect(failure=failure, wall_times=(1700000000.75, 1700000000.25))
                self.assertEqual(sample['collectionWindow'], {'startedAtUnixMs': 1700000000750, 'completedAtUnixMs': 1700000000250})


class NativeCollectorPausedTests(unittest.TestCase):
    def test_paused_container_is_sampled_without_requesting_running_task_inspection(self):
        import threading
        from unittest.mock import patch
        spec=importlib.util.spec_from_file_location('native_paused_subject',SOURCE)
        module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
        observer=module.NativeObserver('/manifest','/output',1);observer.root=Path('/private');observer.host_id='host';observer._stop=threading.Event()
        cid='a'*64
        labels={'centaeris.managed':'true','centaeris.environment_id':'env','centaeris.instance_id':'instance','centaeris.environment_profile_digest':'sha256:'+'b'*64}
        facts={'id':cid,'image':'sha256:'+'c'*64,'state':{'Running':True,'Paused':True},'labels':labels,'limits':{'Runtime':'runsc','Memory':4*1024**3,'CpuQuota':200000,'CpuPeriod':100000,'PidsLimit':512}}
        observer._docker=lambda args,deadline: cid if args[0]=='ps' else __import__('json').dumps(facts)
        requests=[]
        def rpc(request,deadline):
            requests.append(request)
            if request['operation']=='inspect':raise ValueError('running-only inspection rejects paused container')
            return {'kind':'resourceObservation','observation':{'schema':'native.host.resources.v1','hostId':'host','bootId':'test-boot','observedAtBootMs':1000,
                'containers':[{'state':'unknown','target':request['targets'][0],'reason':'test_unknown'}]}}
        observer._rpc=rpc
        class KernelFile:
            def __init__(self,path):self.path=path
            def read_text(self):return 'test-boot' if self.path.endswith('boot_id') else '1.0 1.0'
        with patch.object(module,'Path',KernelFile):sample=observer._sample()
        self.assertEqual([request['operation'] for request in requests],['observeResources'])
        self.assertTrue(sample['states'][cid]['paused']);self.assertIsNone(sample['states'][cid]['inspection'])

class NativeCollectorDiagnosticTests(unittest.TestCase):
    def collect_timeout(self, operation, io_stage='receive', target_count=2, response=None):
        import io
        import json
        from unittest.mock import patch
        spec=importlib.util.spec_from_file_location('native_diagnostic_subject',SOURCE)
        module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
        observer=module.NativeObserver('/manifest','/output',target_count)
        observer.root=Path('/private-sensitive-root');observer.host_id='sensitive-host'
        class MemoryOutput:
            def open(self,*args,**kwargs):return io.StringIO()
        observer.output=MemoryOutput()
        ids=[format(index+1,'064x') for index in range(target_count)]
        clock=[100.0];requests=[]
        def docker(args,deadline):
            clock[0]+=.125
            if args[0]=='ps':return ' '.join(ids)
            return '\n'.join(json.dumps({'id':cid,'image':'sha256:'+'a'*64,
                'state':{'Running':True,'Paused':False},
                'labels':{'centaeris.managed':'true','centaeris.environment_id':'sensitive-environment',
                    'centaeris.instance_id':'sensitive-instance','centaeris.environment_profile_digest':'sha256:'+'b'*64,
                    'centaeris.agent_run_id':'sensitive-run','private-label':'sensitive-label'},
                'limits':{'Runtime':'runsc','Memory':4*1024**3,'CpuQuota':200000,'CpuPeriod':100000,'PidsLimit':512}})
                for cid in args[3:])
        observer._docker=docker
        planned=iter(['inspect']*target_count+['observeResources'])
        class Socket:
            def __init__(self):self.operation=next(planned)
            def __enter__(self):return self
            def __exit__(self,*args):return False
            def settimeout(self,value):pass
            def step(self,stage):
                clock[0]+=.25
                if self.operation==operation and stage==io_stage and response is None:
                    observer._stop.set()
                    raise TimeoutError('sensitive-error token=password request-body')
            def connect(self,path):self.step('connect')
            def sendall(self,data):
                requests.append(json.loads(data));self.step('send')
            def recv(self,size):
                self.step('receive')
                if self.operation==operation and response is not None:
                    observer._stop.set()
                    return json.dumps(response).encode()+b'\n'
                return json.dumps({'schema':'environment.host.v1','status':'success',
                    'result':{'kind':'inspection','inspection':{'schema':'environment.task.store.inspection.v2',
                        'instanceId':'sensitive-instance','activeTasks':1}}}).encode()+b'\n'
        with patch.object(module.time,'monotonic',side_effect=lambda:clock[0]), \
                patch.object(module.socket,'AF_UNIX',1,create=True), \
                patch.object(module.socket,'socket',side_effect=lambda *_:Socket()):
            observer._loop()
        self.assertEqual(len(observer.samples),1)
        return module,observer.samples[0],requests

    def test_timeout_diagnostics_distinguish_task_inspection_from_resource_observation(self):
        import json
        for operation,stage,completed in [('inspect','taskInspection',0),('observeResources','resourceObservation',2)]:
            with self.subTest(operation=operation):
                _,sample,requests=self.collect_timeout(operation)
                self.assertEqual(sample['state'],'unknown')
                self.assertEqual(sample['reason'],'resource_probe_timeouterror')
                self.assertEqual(set(sample),{'schema','state','reason','diagnostic','collectionWindow'})
                diagnostic=sample['diagnostic']
                self.assertEqual(set(diagnostic),{'stage','operation','ioStage','elapsedMs','remainingMs',
                    'targetCount','completedTargetCount','exceptionType'})
                self.assertEqual(diagnostic['stage'],stage)
                self.assertEqual(diagnostic['operation'],operation)
                self.assertEqual(diagnostic['ioStage'],'receive')
                self.assertEqual(diagnostic['exceptionType'],'TimeoutError')
                self.assertEqual(diagnostic['targetCount'],2)
                self.assertEqual(diagnostic['completedTargetCount'],completed)
                self.assertGreater(diagnostic['elapsedMs'],0)
                self.assertGreaterEqual(diagnostic['remainingMs'],0)
                self.assertLessEqual(abs(diagnostic['elapsedMs']+diagnostic['remainingMs']-8000),1)
                self.assertTrue(requests)
                encoded=json.dumps(sample)
                for secret in ('sensitive-','password','request-body','targets','environmentInstanceId','private-label'):
                    self.assertNotIn(secret,encoded)

    def test_timeout_diagnostics_keep_connect_and_send_failures_distinct(self):
        for io_stage in ('connect','send'):
            with self.subTest(io_stage=io_stage):
                _,sample,_=self.collect_timeout('inspect',io_stage)
                self.assertEqual(sample['reason'],'resource_probe_timeouterror')
                self.assertEqual(sample['diagnostic']['stage'],'taskInspection')
                self.assertEqual(sample['diagnostic']['ioStage'],io_stage)
                self.assertEqual(sample['diagnostic']['completedTargetCount'],0)

    def test_completed_targets_on_failed_sample_are_diagnostic_only(self):
        module,sample,_=self.collect_timeout('observeResources',target_count=4)
        self.assertEqual(sample['diagnostic']['completedTargetCount'],4)
        self.assertNotIn('states',sample);self.assertNotIn('host',sample)
        report=module.summarize([sample,sample,sample],4)
        self.assertEqual(report['physicalOverlap'],'notObserved')
        self.assertEqual(report['peakWorkingEnvironments'],0)
        self.assertEqual(report['consecutiveWorkingIntervals'],0)
        self.assertEqual(report['unknownReasons'],['resource_probe_timeouterror'])

    def test_response_validation_distinguishes_host_error_schema_and_result_kind(self):
        import json
        cases=[({'schema':'environment.host.v1','status':'error','error':'host_runtime_identity_rejected'},
                'hostError','host_runtime_identity_rejected'),
               ({'schema':'unsupported-sensitive-schema','status':'error','error':'sensitive-error'},
                'schemaMismatch',None),
               ({'schema':'environment.host.v1','status':'success','result':{'kind':'sensitive-kind'}},
                'resultKindMismatch',None),
               ({'schema':'environment.host.v1','status':'sensitive-status'},'statusInvalid',None)]
        for response,failure,code in cases:
            with self.subTest(failure=failure):
                module,sample,_=self.collect_timeout('inspect',target_count=4,response=response)
                self.assertEqual(sample['state'],'unknown')
                self.assertEqual(sample['reason'],'resource_probe_valueerror')
                diagnostic=sample['diagnostic']
                self.assertEqual(diagnostic['stage'],'taskInspection')
                self.assertEqual(diagnostic['ioStage'],'validate')
                self.assertEqual(diagnostic['completedTargetCount'],0)
                self.assertEqual(diagnostic['responseFailure'],failure)
                self.assertEqual(diagnostic.get('hostErrorCode'),code)
                self.assertNotIn('sensitive',json.dumps(sample))
                self.assertNotIn('states',sample);self.assertNotIn('host',sample)
                report=module.summarize([sample,sample,sample],4)
                self.assertEqual(report['physicalOverlap'],'notObserved')
                self.assertEqual(report['peakWorkingEnvironments'],0)
                self.assertEqual(report['consecutiveWorkingIntervals'],0)

    def test_resource_result_kind_failure_retains_completed_checks_without_overlap(self):
        module,sample,_=self.collect_timeout('observeResources',target_count=4,response={
            'schema':'environment.host.v1','status':'success','result':{'kind':'inspection'}})
        self.assertEqual(sample['state'],'unknown')
        self.assertEqual(sample['diagnostic']['responseFailure'],'resultKindMismatch')
        self.assertEqual(sample['diagnostic']['stage'],'sampleValidation')
        self.assertEqual(sample['diagnostic']['completedTargetCount'],4)
        report=module.summarize([sample,sample,sample],4)
        self.assertEqual(report['physicalOverlap'],'notObserved')
        self.assertEqual(report['peakWorkingEnvironments'],0)

    def test_host_error_codes_use_a_finite_allowlist_without_retaining_unknown_text(self):
        import json
        for error,expected in [('host_path_unavailable','host_path_unavailable'),
                               ('task_index_unavailable','task_index_unavailable'),
                               ('host_probe_busy','host_probe_busy'),
                               ('host_private_sensitive_token','unknown'),
                               ('host_runtime_identity_rejected token=password','unknown'),
                               ({'password':'sensitive-secret'},'unknown')]:
            with self.subTest(expected=expected):
                _,sample,_=self.collect_timeout('inspect',response={
                    'schema':'environment.host.v1','status':'error','error':error})
                self.assertEqual(sample['state'],'unknown')
                self.assertEqual(sample['diagnostic']['responseFailure'],'hostError')
                self.assertEqual(sample['diagnostic']['hostErrorCode'],expected)
                encoded=json.dumps(sample)
                for secret in ('sensitive','password','private_sensitive_token','token=','environmentInstanceId'):
                    self.assertNotIn(secret,encoded)


class NativeBatchInventoryTests(unittest.TestCase):
    def collector(self, count=8, *, inventory=None, batch=None, failure=None):
        import json
        from unittest.mock import Mock
        module = NativeObservationTests().subject()
        observer = module.NativeObserver('/manifest', '/output', max(count, 1))
        observer.root = Path('/private'); observer.host_id = 'host'
        ids = [format(index + 1, '064x') for index in range(count)]
        facts = [{'id':cid, 'image':'sha256:'+'a'*64,
            'state':{'Running':True,'Paused':False},
            'labels':{'centaeris.managed':'true','centaeris.environment_id':f'env-{index}',
                'centaeris.instance_id':f'instance-{index}',
                'centaeris.environment_profile_digest':'sha256:'+'b'*64},
            'limits':{'Runtime':'runsc','Memory':4*1024**3,'CpuQuota':200000,'CpuPeriod':100000,'PidsLimit':512}}
            for index,cid in enumerate(ids)]
        lines = facts if batch is None else batch(facts)
        def docker(args, deadline):
            if args[0] == 'ps':
                return ' '.join(ids if inventory is None else inventory(ids))
            if failure is not None:
                raise failure
            return lines if isinstance(lines, str) else '\n'.join(json.dumps(row) for row in lines)
        observer._docker = Mock(side_effect=docker)
        def rpc(request, deadline):
            if request['operation'] == 'inspect':
                return {'kind':'inspection','inspection':{'schema':'environment.task.store.inspection.v2',
                    'instanceId':request['environmentInstanceId'],'activeTasks':1}}
            return {'kind':'resourceObservation','observation':{'schema':'native.host.resources.v1',
                'hostId':'host','bootId':'test-boot','observedAtBootMs':1000,
                'containers':[{'state':'unknown','target':target,'reason':'test_unknown'} for target in request['targets']]}}
        observer._rpc = Mock(side_effect=rpc)
        class KernelFile:
            def __init__(self, path): self.path=path
            def read_text(self): return 'test-boot' if self.path.endswith('boot_id') else '1.0 1.0'
        return module, observer, ids, KernelFile

    def sample(self, observer, module, kernel):
        from unittest.mock import patch
        with patch.object(module, 'Path', kernel):
            return observer._sample()

    def failed_sample(self, observer, module, kernel):
        import io
        from unittest.mock import patch
        class MemoryOutput:
            def open(self, *args, **kwargs): return io.StringIO()
        observer.output = MemoryOutput()
        collect = observer._sample
        def once():
            try: return collect()
            finally: observer._stop.set()
        with patch.object(observer, '_sample', side_effect=once), patch.object(module, 'Path', kernel):
            observer._loop()
        return observer.samples[0]

    def test_eight_targets_use_one_exact_batch_before_serial_task_and_resource_rpcs(self):
        module, observer, ids, kernel = self.collector(batch=lambda rows:list(reversed(rows)))
        sample = self.sample(observer, module, kernel)
        inspections = [call for call in observer._docker.call_args_list if call.args[0][0]=='inspect']
        self.assertEqual(len(inspections), 1)
        self.assertEqual(inspections[0].args[0][3:], ids)
        self.assertEqual(set(sample['states']), set(ids))
        requests = [call.args[0] for call in observer._rpc.call_args_list]
        self.assertEqual([request['operation'] for request in requests], ['inspect']*8+['observeResources'])
        self.assertEqual([request['environmentInstanceId'] for request in requests[:-1]],
                         [f'instance-{index}' for index in range(8)])
        deadlines = [call.args[1] for call in observer._docker.call_args_list+observer._rpc.call_args_list]
        self.assertEqual(len(set(deadlines)), 1)

    def test_entire_batch_is_validated_before_any_task_rpc(self):
        def foreign(rows):
            rows[-1]['id']='f'*64
            return rows
        def invalid_caps(rows):
            rows[-1]['limits']['Memory']=1
            return rows
        def invalid_instance(rows):
            rows[-1]['labels']['centaeris.instance_id']=''
            return rows
        def invalid_running(rows):
            rows[-1]['state']['Running']='true'
            return rows
        def extra_id(rows):
            extra=copy.deepcopy(rows[-1]);extra['id']='f'*64
            return rows+[extra]
        for mutate in (lambda rows:rows[:-1], lambda rows:rows+[rows[0]], foreign,
                       invalid_caps, invalid_instance, invalid_running, extra_id,
                       lambda rows:'private-invalid-json'):
            with self.subTest(mutate=mutate):
                module, observer, _, kernel = self.collector(batch=mutate)
                sample = self.failed_sample(observer, module, kernel)
                self.assertEqual(sample['state'], 'unknown')
                self.assertEqual(sample['diagnostic']['stage'], 'dockerInspect')
                self.assertEqual(sample['diagnostic']['completedTargetCount'], 0)
                self.assertNotIn('host', sample)
                observer._rpc.assert_not_called()
                self.assertNotIn('private', __import__('json').dumps(sample))

    def test_partial_batch_subprocess_failure_never_uses_returned_targets(self):
        import subprocess
        failure = subprocess.CalledProcessError(1, ['docker'], output='private partial facts')
        module, observer, _, kernel = self.collector(failure=failure)
        sample = self.failed_sample(observer, module, kernel)
        self.assertEqual(sample['state'], 'unknown')
        self.assertEqual(sample['reason'], 'resource_probe_calledprocesserror')
        observer._rpc.assert_not_called()
        self.assertNotIn('private', __import__('json').dumps(sample))

    def test_duplicate_inventory_is_rejected_before_inspection(self):
        module, observer, _, kernel = self.collector(inventory=lambda ids:ids+[ids[0]])
        sample = self.failed_sample(observer, module, kernel)
        self.assertEqual(sample['state'], 'unknown')
        self.assertEqual(observer._docker.call_count, 1)
        observer._rpc.assert_not_called()

    def test_empty_inventory_skips_docker_inspect_but_observes_the_host(self):
        module, observer, _, kernel = self.collector(count=0)
        sample = self.sample(observer, module, kernel)
        self.assertEqual(observer._docker.call_count, 1)
        observer._rpc.assert_called_once()
        self.assertEqual(observer._rpc.call_args.args[0]['targets'], [])
        self.assertEqual(sample['states'], {})
class NativeResourceResponseDeadlineTests(unittest.TestCase):
    def transport(self, operation, chunks, *, deadline=108.0):
        import json
        from unittest.mock import patch
        module = NativeObservationTests().subject()
        observer = module.NativeObserver('/manifest', '/output', 8)
        observer.root = Path('/private')
        clock = [100.0]
        result = {'kind':'resourceObservation' if operation == 'observeResources' else 'inspection'}
        response = json.dumps({'schema':'environment.host.v1', 'status':'success', 'result':result}).encode()
        pending = iter((delay, response if data is None else data) for delay, data in chunks)
        class Socket:
            def __init__(self):
                self.timeout = None; self.receive_timeouts = []; self.receives = 0
                self.connect_timeout = None; self.send_timeout = None
            def __enter__(self): return self
            def __exit__(self, *args): return False
            def settimeout(self, value): self.timeout = value
            def connect(self, path): self.connect_timeout = self.timeout
            def sendall(self, data): self.send_timeout = self.timeout
            def recv(self, size):
                self.receives += 1; self.receive_timeouts.append(self.timeout)
                delay, data = next(pending)
                if delay > self.timeout:
                    clock[0] += self.timeout
                    raise TimeoutError('simulated response timeout')
                clock[0] += delay
                return data
        connection = Socket()
        def invoke():
            with patch.object(module.time, 'monotonic', side_effect=lambda:clock[0]), \
                    patch.object(module.socket, 'AF_UNIX', 1, create=True), \
                    patch.object(module.socket, 'socket', return_value=connection):
                return observer._rpc({'operation':operation}, deadline)
        return invoke, connection, clock, result

    def test_resource_response_after_two_seconds_uses_remaining_sample_budget(self):
        invoke, connection, clock, result = self.transport('observeResources', [(2.2, None), (0, b'\n')])
        self.assertEqual(invoke(), result)
        self.assertEqual(connection.connect_timeout, 2)
        self.assertEqual(connection.send_timeout, 2)
        self.assertAlmostEqual(clock[0], 102.2)
        self.assertAlmostEqual(connection.receive_timeouts[0], 8)

    def test_fragmented_response_does_not_refresh_the_absolute_deadline(self):
        invoke, connection, clock, _ = self.transport('observeResources',
            [(2.2, b'{'), (2.2, b'"schema"'), (3.7, b':')])
        with self.assertRaises(TimeoutError): invoke()
        self.assertAlmostEqual(clock[0], 108)
        self.assertEqual(connection.receives, 3)
        for actual, expected in zip(connection.receive_timeouts, (8, 5.8, 3.6)):
            self.assertAlmostEqual(actual, expected)

    def test_expired_deadline_prevents_the_next_receive(self):
        invoke, connection, clock, _ = self.transport('observeResources', [(8, b'{')])
        with self.assertRaises(TimeoutError): invoke()
        self.assertAlmostEqual(clock[0], 108)
        self.assertEqual(connection.receives, 1)
        invoke, connection, _, _ = self.transport('observeResources', [], deadline=100)
        with self.assertRaises(TimeoutError): invoke()
        self.assertEqual(connection.receives, 0)

    def test_ordinary_task_inspection_keeps_its_two_second_response_limit(self):
        invoke, connection, clock, _ = self.transport('inspect', [(2.2, None)])
        with self.assertRaises(TimeoutError): invoke()
        self.assertAlmostEqual(clock[0], 102)
        self.assertEqual(connection.receive_timeouts, [2])
        self.assertEqual(connection.connect_timeout, 2)
        self.assertEqual(connection.send_timeout, 2)

    def test_completed_resource_response_at_expiry_cannot_be_accepted(self):
        invoke, connection, clock, _ = self.transport('observeResources', [(0, None), (8, b'\n')])
        with self.assertRaises(TimeoutError): invoke()
        self.assertAlmostEqual(clock[0], 108)
        self.assertEqual(connection.receives, 2)


if __name__=='__main__':unittest.main()
