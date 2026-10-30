"""Read-only private native resource observations; never admission or release authority."""
import importlib.util
import json
from pathlib import Path
import re
import socket
import subprocess
import threading
import time


# Only fixed public error codes may enter diagnostics; never retain host error text.
_HOST_ERROR_CODES=frozenset({
    'host_request_invalid','host_schema_unsupported','host_target_invalid',
    'host_path_unavailable','host_path_untrusted','host_state_unavailable',
    'host_state_invalid','host_state_oversized','host_registered_target_changed',
    'host_retirement_pending','host_target_unknown','host_target_identity_rejected',
    'host_runtime_identity_rejected','host_power_unknown','host_command_failed',
    'host_command_timeout','host_command_response_invalid','host_probe_timeout',
    'host_probe_failed','host_probe_cancelled','host_probe_busy','task_index_unavailable',
    'task_index_invalid','task_index_unsupported_schema',
})


def _module(name, path):
    spec=importlib.util.spec_from_file_location(name,path)
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module


def _working(sample):
    host=sample.get('host',{})
    if host.get('schema')!='native.host.resources.v1':return {}
    result={};environments=set()
    for item in host.get('containers',[]):
        if item.get('state')!='observed':continue
        resource=item['resources'];target=resource['target'];container=target['containerId']
        state=sample.get('states',{}).get(container,{})
        inspection=state.get('inspection',{})
        environment=target['environmentId']
        if (state.get('caps')!={'memoryBytes':4*1024**3,'cpuMilli':2000,'pids':512}
                or not state.get('running') or state.get('paused') is not False
                or inspection.get('schema')!='environment.task.store.inspection.v2'
                or inspection.get('instanceId')!=target['environmentInstanceId']
                or type(inspection.get('activeTasks')) is not int or inspection['activeTasks']<=0):continue
        if environment in environments:return {}
        environments.add(environment)
        identity=(json.dumps(target,sort_keys=True),resource['initPid'],resource['initStartTicks'],
                  json.dumps(resource['cgroupIdentity'],sort_keys=True))
        result[environment]=(identity,resource['cpuCounters']['usageUsec'])
    return result


def summarize(samples, expected_environments, *, expected_working_environments=None):
    working_target = expected_environments if expected_working_environments is None else expected_working_environments
    if type(working_target) is not int or not 1 <= working_target <= expected_environments:
        raise ValueError('working environment target must be within inventory count')
    peak=0;streak=0;longest=0;previous=None;previous_work={};common=set();reasons=set();paused=set();resumed=set();seen_paused=set()
    for sample in samples:
        if sample.get('state')=='unknown':
            reasons.add(sample.get('reason','sample_unknown'));previous=None;previous_work={};streak=0;common=set();continue
        host=sample.get('host',{})
        for item in host.get('containers',[]):
            if item.get('state')=='unknown':reasons.add(item.get('reason','container_unknown'))
        work=_working(sample)
        for item in host.get('containers',[]):
            target=item.get('resources',item).get('target',{})
            key=(target.get('environmentId'),target.get('environmentInstanceId'))
            state=sample.get('states',{}).get(target.get('containerId'),{})
            if state.get('paused') is True:paused.add(key);seen_paused.add(key)
            elif key in seen_paused and state.get('running') and state.get('paused') is False:resumed.add(key)
        working=set()
        if (previous and host.get('bootId')==previous.get('bootId') and host.get('hostId')==previous.get('hostId')
                and 0 < host.get('observedAtBootMs',0)-previous.get('observedAtBootMs',0) <= 5000):
            for environment,(identity,cpu) in work.items():
                old=previous_work.get(environment)
                if old and old[0]==identity and cpu-old[1]>=100000:working.add(environment)
        peak=max(peak,len(working))
        if len(working)>=working_target:
            overlap=common & working
            if streak and len(overlap)>=working_target:streak+=1;common=overlap
            else:streak=1;common=working
        else:streak=0;common=set()
        longest=max(longest,streak);previous=host;previous_work=work
    return {'expectedEnvironments':expected_environments,'expectedWorkingEnvironments':working_target,
            'physicalOverlap':'verified' if longest>=2 else 'notObserved','peakWorkingEnvironments':peak,
            'consecutiveWorkingIntervals':longest,'unknownReasons':sorted(reasons),'sampleCount':len(samples),
            'pausedEnvironmentInstances':len(paused),'resumedEnvironmentInstances':len(resumed)}


class NativeObserver:
    """One-second sampling in an already-owned netns. stop always joins its thread."""
    def __init__(self,native_manifest,output,expected_environments,*,expected_working_environments=None):
        if type(expected_environments) is not int or not 1<=expected_environments<=64:raise ValueError('expected environment count must be 1..64')
        self.manifest=Path(native_manifest);self.output=Path(output);self.expected=expected_environments
        self.expected_working=expected_environments if expected_working_environments is None else expected_working_environments
        if type(self.expected_working) is not int or not 1<=self.expected_working<=expected_environments:raise ValueError('working environment target must be within inventory count')
        self.samples=[];self._stop=threading.Event();self._thread=None;self._progress=None

    def _probe_stage(self,stage,operation,io_stage):
        self._progress.update(stage=stage,operation=operation,ioStage=io_stage)

    def _probe_io(self,io_stage):
        if self._progress is not None:self._progress['ioStage']=io_stage

    def _probe_diagnostic(self,error):
        progress=self._progress
        if progress is None:return None
        now=time.monotonic()
        # Timings cover this entire sample; response details are fixed categories/codes.
        diagnostic={'stage':progress['stage'],'operation':progress['operation'],'ioStage':progress['ioStage'],
                'elapsedMs':max(0,int((now-progress['started'])*1000)),
                'remainingMs':max(0,int((progress['deadline']-now)*1000)),
                'targetCount':progress['targetCount'],'completedTargetCount':progress['completedTargetCount'],
                'exceptionType':type(error).__name__}
        for key in ('responseFailure','hostErrorCode'):
            if key in progress:diagnostic[key]=progress[key]
        return diagnostic

    def _response_failure(self,failure,host_error=None):
        if self._progress is not None:
            self._progress['responseFailure']=failure
            if failure=='hostError':
                self._progress['hostErrorCode']=(host_error if type(host_error) is str
                    and host_error in _HOST_ERROR_CODES else 'unknown')
        raise ValueError('host observation unavailable')

    def _docker(self,args,deadline):
        timeout=min(4,max(.1,deadline-time.monotonic()))
        value=subprocess.run(['docker','--host','unix://'+str(self.root/'docker.sock'),*args],stdin=subprocess.DEVNULL,
                             capture_output=True,text=True,timeout=timeout,check=True).stdout
        if len(value)>4*1024*1024:raise ValueError('bounded Docker observation required')
        return value

    def _rpc(self,request,deadline):
        self._probe_io('connect')
        with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as connection:
            connection.settimeout(min(2,max(.1,deadline-time.monotonic())))
            connection.connect(str(self.root/'host/control.sock'))
            self._probe_io('send')
            connection.sendall(json.dumps({'schema':'environment.host.v1','request':request}).encode()+b'\n')
            data=bytearray()
            self._probe_io('receive')
            resource_response=request.get('operation')=='observeResources'
            while not data.endswith(b'\n'):
                if time.monotonic()>=deadline:raise TimeoutError('host observation deadline')
                if resource_response:
                    remaining=deadline-time.monotonic()
                    if remaining<=0:raise TimeoutError('host observation deadline')
                    connection.settimeout(remaining)
                else:
                    connection.settimeout(min(2,max(.1,deadline-time.monotonic())))
                chunk=connection.recv(65536)
                if resource_response and time.monotonic()>=deadline:raise TimeoutError('host observation deadline')
                if not chunk:raise ValueError('truncated host observation')
                data.extend(chunk)
                if len(data)>4*1024*1024:raise ValueError('bounded host observation required')
        self._probe_io('decode')
        result=json.loads(data)
        self._probe_io('validate')
        if not isinstance(result,dict) or result.get('schema')!='environment.host.v1':
            self._response_failure('schemaMismatch')
        if result.get('status')=='error':self._response_failure('hostError',result.get('error'))
        if result.get('status')!='success':self._response_failure('statusInvalid')
        return result['result']

    def _sample(self):
        started=time.monotonic();deadline=started+8
        # targetCount is inventory size; completedTargetCount counts Docker/task checks,
        # never completed resource observations.
        self._progress={'started':started,'deadline':deadline,'targetCount':0,'completedTargetCount':0}
        self._probe_stage('dockerInventory','ps','subprocess')
        ids=self._docker(['ps','-aq','--no-trunc','--filter','label=centaeris.managed=true'],deadline).split()
        self._progress['targetCount']=len(ids)
        self._probe_io('validate')
        if (len(ids)>64 or len(set(ids))!=len(ids)
                or any(not re.fullmatch('[a-f0-9]{64}',item) for item in ids)):
            raise ValueError('bounded exact managed inventory required')
        states={};targets=[];prepared={}
        template='{"id":{{json .Id}},"image":{{json .Image}},"state":{{json .State}},"labels":{{json .Config.Labels}},"limits":{{json .HostConfig}}}'
        if ids:
            self._probe_stage('dockerInspect','inspect','subprocess')
            if self._stop.is_set():raise InterruptedError('observer stopped')
            raw=self._docker(['inspect','--format',template,*ids],deadline)
            self._probe_io('decode')
            facts_batch=[json.loads(line) for line in raw.splitlines() if line.strip()]
            self._probe_io('validate')
            for facts in facts_batch:
                container=facts['id'];labels=facts['labels'];limits=facts['limits']
                if (container not in ids or container in prepared
                        or labels.get('centaeris.managed')!='true' or limits['Runtime']!='runsc'):
                    raise ValueError('managed target mismatch')
                if limits['Memory']!=4*1024**3 or limits['CpuPeriod']!=100000 or limits['CpuQuota']!=200000 or limits['PidsLimit']!=512:
                    raise ValueError('default workload cap mismatch')
                target={'containerId':container,'environmentId':labels['centaeris.environment_id'],
                        'environmentInstanceId':labels['centaeris.instance_id'],'profileDigest':labels['centaeris.environment_profile_digest'],
                        'imageDigest':facts['image']}
                if (any(not isinstance(value,str) or not value for value in target.values())
                        or type(facts['state']['Running']) is not bool or type(facts['state']['Paused']) is not bool):
                    raise ValueError('managed target identity unavailable')
                prepared[container]=(facts,target)
            if set(prepared)!=set(ids):raise ValueError('exact managed inspection inventory required')
        for container in ids:
            if self._stop.is_set():raise InterruptedError('observer stopped')
            facts,target=prepared[container];labels=facts['labels']
            inspection=None
            if facts['state']['Running'] and not facts['state']['Paused']:
                self._probe_stage('taskInspection','inspect','connect')
                response=self._rpc({'operation':'inspect','environmentInstanceId':target['environmentInstanceId']},deadline)
                if not isinstance(response,dict) or response.get('kind')!='inspection':self._response_failure('resultKindMismatch')
                inspection=response['inspection']
            targets.append(target)
            states[container]={'running':facts['state']['Running'],'paused':facts['state']['Paused'],
                               'inspection':inspection,'caps':{'memoryBytes':facts['limits']['Memory'],
                               'cpuMilli':facts['limits']['CpuQuota']*1000//facts['limits']['CpuPeriod'],
                               'pids':facts['limits']['PidsLimit']},'agentRunId':labels.get('centaeris.agent_run_id')}
            self._progress['completedTargetCount']=len(targets)
        self._probe_stage('resourceObservation','observeResources','connect')
        result=self._rpc({'operation':'observeResources','expectedHostId':self.host_id,'targets':targets},deadline)
        self._probe_stage('sampleValidation','validate','validate')
        if not isinstance(result,dict) or result.get('kind')!='resourceObservation':self._response_failure('resultKindMismatch')
        observation=result['observation']
        if (observation.get('schema')!='native.host.resources.v1' or observation.get('hostId')!=self.host_id
                or observation.get('bootId')!=Path('/proc/sys/kernel/random/boot_id').read_text().strip()
                or len(observation['containers'])!=len(targets)):raise ValueError('host identity/schema mismatch')
        now=int(float(Path('/proc/uptime').read_text().split()[0])*1000)
        if not 0<=now-observation['observedAtBootMs']<=5000:raise ValueError('host observation stale')
        observed=[item.get('resources',item)['target'] for item in observation['containers']]
        if sorted(map(lambda item:json.dumps(item,sort_keys=True),observed))!=sorted(map(lambda item:json.dumps(item,sort_keys=True),targets)):
            raise ValueError('host observation target mismatch')
        return {'schema':'workspace.perf.native_sample.v1','host':observation,'states':states}

    def start(self):
        if self._thread is not None:raise RuntimeError('observer already started')
        fixture=_module('native_observer_fixture',Path(__file__).with_name('fixture_workload.py'))
        fixture.verify_native_fixture(self.manifest)
        native=json.loads(self.manifest.read_text());self.root=Path(native['isolationOwner']).parent
        workload=native.get('sharedWorkload') or {}
        if workload.get('capMemoryBytes')!=4*1024**3 or workload.get('capCpuMilli')!=2000:
            raise ValueError('default shared workload caps required')
        self.output=fixture._external(self.output)
        if self.output.exists():raise ValueError('new external observation file required')
        self.host_id=self._docker(['info','--format','{{json .ID}}'],time.monotonic()+4).strip().strip('"')
        if not self.host_id:raise ValueError('private Docker host identity required')
        self._thread=threading.Thread(target=self._loop,name='native-perf-observer',daemon=True);self._thread.start();return self

    def _loop(self):
        with self.output.open('x',encoding='utf-8') as output:
            while not self._stop.is_set():
                started=time.monotonic()
                self._progress=None
                collection_started=int(time.time()*1000)
                try:sample=self._sample()
                except Exception as error:
                    sample={'schema':'workspace.perf.native_sample.v1','state':'unknown','reason':'resource_probe_'+type(error).__name__.lower()}
                    diagnostic=self._probe_diagnostic(error)
                    if diagnostic is not None:sample['diagnostic']=diagnostic
                # Preserve raw wall times, including rollback; consumers must reject inverted windows.
                sample['collectionWindow']={'startedAtUnixMs':collection_started,'completedAtUnixMs':int(time.time()*1000)}
                self.samples.append(sample);output.write(json.dumps(sample)+'\n');output.flush()
                if len(self.samples)>=1800:self._stop.set();break
                self._stop.wait(max(0,1-(time.monotonic()-started)))

    def stop(self):
        if self._thread is None:raise RuntimeError('observer not started')
        self._stop.set();self._thread.join(timeout=15)
        if self._thread.is_alive():raise RuntimeError('observer thread remains active')
        return summarize(self.samples,self.expected,expected_working_environments=self.expected_working)
