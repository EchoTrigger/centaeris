"""Exercise an already prepared owned fixture; no global cleanup or auto-teardown.

The caller authorizes destruction only of this generated fixture's exact retained
instance. Private credentials are read locally and never included in evidence.
"""
import argparse
from collections import deque
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time
import urllib.request
import uuid
from workload import instructions_facts
from progress import probe_reclaimed_controller, compaction_recovery_proof
from observe import task_rpc
import boundary
from cross_retry import wait_for_release, load_resume


def append_command_evidence(path, sequence, argv, result):
    limit=1024*1024
    row={'sequence':sequence,'command':argv,'exitCode':result.returncode,'outputTruncated':False}
    for name in ('stdout','stderr'):
        data=getattr(result,name).encode()
        row[name+'Bytes']=len(data)
        row[name+'Sha256']=hashlib.sha256(data).hexdigest()
        row[name]=data[:limit].decode(errors='replace')
        row['outputTruncated']|=len(data)>limit
    with path.open('a',encoding='utf-8') as stream:
        stream.write(json.dumps(row)+'\n')
    if row['outputTruncated']:
        raise RuntimeError('command evidence output exceeded1MiB per stream; bounded diagnostic preserved')


class ProgressEvidence:
    """Retain a bounded window and monotonic aggregates over every observation."""
    def __init__(self):
        self.samples=deque(maxlen=256)
        self.count=0
        self.maxima={}

    def record(self,sample):
        self.count+=1
        self.samples.append(sample)
        for section in ('store','live'):
            for key,value in sample.get(section,{}).items():
                if type(value) is int:
                    name=section+'.'+key
                    self.maxima[name]=max(value,self.maxima.get(name,0))

    def snapshot(self):
        return {'observationCount':self.count,'retainedSampleLimit':256,
            'retainedSamples':list(self.samples),'maxima':self.maxima}


def retryable_observation(result):
    if result.returncode!=75:return False
    try:
        value=json.loads(result.stderr.strip().splitlines()[-1])
    except (ValueError,IndexError):return False
    return (isinstance(value,dict) and value.get('schema')=='retained.acceptance.observation.v1'
        and value.get('status')=='notReady' and isinstance(value.get('reason'),str))


def history_metrics(state):
    facts=state.get('historyObservation')
    if (state.get('runStatus')!='completed'
            or (state.get('coreTerminal') or {}).get('type')!='agent_run_completed'
            or facts!={'attemptedCommands':70,'completedCommands':70,'distinctCallIds':70}):
        raise RuntimeError('same-Run history lacks70 distinct actual successful commands')
    return facts


def should_restart_long_run(sample, previous_restart):
    if previous_restart is not None or sample['completedCommands']<100:return False
    if sample.get('runStatus')!='running' or sample.get('fence',{}).get('controllerPurpose')!='execute':return False
    summary=sample.get('durableCompaction')
    if summary is None:return False
    request=summary.get('requestBound');main=summary.get('lastMainObservation')
    if request is None or main is None:return False
    import re
    if (summary.get('failureCount')!=0 or not summary.get('compactionId')
            or not summary.get('summaryMessageId') or not summary.get('sessionEventId')
            or not re.fullmatch(r'sha256:[0-9a-f]{64}',summary.get('summarySha256',''))
            or type(summary.get('summaryBytes')) is not int or summary['summaryBytes']<=0
            or type(request.get('sequence')) is not int or type(summary.get('sequence')) is not int
            or not request.get('requestId') or not main.get('requestId')
            or not re.fullmatch(r'sha256:[0-9a-f]{64}',request.get('manifestDigest',''))
            or not re.fullmatch(r'sha256:[0-9a-f]{64}',request.get('compactionPromptDigest',''))
            or not request['sequence']<summary['sequence']<main.get('sequence',0)<=sample.get('highestSessionSequence',0)):
        raise RuntimeError('long Run durable compaction proof is invalid or failed')
    return True


def boundary_goal(steps,nonce,timeout,long_run):
    if steps is None:return None
    if long_run:raise ValueError('boundary and1000-command modes are mutually exclusive')
    return boundary.marker_for(steps,nonce,timeout)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--fixture', required=True, type=Path)
    modes=parser.add_mutually_exclusive_group()
    modes.add_argument('--long-run',action='store_true',help='separate1000-command same-Run gate; use a fresh fixture')
    modes.add_argument('--boundary-steps',type=int,choices=range(8,33),help='explicit8..32 command running-task restart gate; fresh fixture required')
    modes.add_argument('--resume-cross-evidence',type=Path,help='resume only this existing cross-Run fixture; never recreate its instance')
    parser.add_argument('--expected-container',help='exact managed container required for cross-Run retry')
    parser.add_argument('--boundary-timeout-seconds',type=int,default=180,choices=range(60,301))
    parser.add_argument('--acknowledge-tmpfs-loss', action='store_true', required=True)
    args = parser.parse_args()
    if args.boundary_steps is None and args.boundary_timeout_seconds!=180:
        parser.error('--boundary-timeout-seconds requires --boundary-steps')
    if bool(args.resume_cross_evidence)!=bool(args.expected_container):
        parser.error('--resume-cross-evidence and --expected-container are required together')
    boundary_marker=boundary_goal(args.boundary_steps,uuid.uuid4().hex,args.boundary_timeout_seconds,args.long_run)
    boundary_config=boundary.parse_marker(boundary_marker) if boundary_marker else None
    root = args.fixture.resolve(strict=True)
    manifest = json.loads((root / 'manifest.json').read_text())
    binaries=Path(manifest['binaryDirectory'])
    if any(hashlib.sha256((binaries / name).read_bytes()).hexdigest()!=digest
           for name,digest in manifest['binarySha256'].items()):
        raise SystemExit('mounted binaries changed since verified preparation')
    source=manifest['source']
    if (subprocess.run(['git','-C',source,'status','--porcelain'],check=True,capture_output=True,text=True).stdout.strip()
            or subprocess.run(['git','-C',source,'rev-parse','HEAD'],check=True,capture_output=True,text=True).stdout.strip()!=manifest['sourceRevision']):
        raise SystemExit('mounted source changed since verified preparation')
    compose = root / 'compose.json'
    config = json.loads(compose.read_text())
    project = manifest['project']
    if config['name'] != project or not project.startswith('retainedaccept'):
        raise SystemExit('fixture project identity rejected')
    if any(service['labels'].get('centaeris.acceptance.project') != project
           for service in config['services'].values()):
        raise SystemExit('fixture service ownership rejected')
    inventory = subprocess.run(['docker','ps','-aq','--no-trunc','--filter','label=centaeris.managed=true'],
                               capture_output=True,text=True,check=True)
    resume=None
    if args.resume_cross_evidence:
        resume=load_resume(root,args.resume_cross_evidence,args.expected_container)
        if inventory.stdout.splitlines()!=[args.expected_container]:
            raise SystemExit('retry requires exactly the authorized managed container')
        actual=subprocess.run(['docker','inspect',args.expected_container],capture_output=True,text=True,check=True)
        rows=json.loads(actual.stdout)
        if len(rows)!=1 or rows[0]['Id']!=args.expected_container or not rows[0]['State']['Running']:
            raise SystemExit('exact retry container is not running')
        labels=rows[0]['Config']['Labels'];old=resume['stateA']
        if any(labels.get(k)!=v for k,v in {'centaeris.managed':'true',
                'centaeris.environment_id':old['environmentId'],'centaeris.instance_id':old['instanceId']}.items()):
            raise SystemExit('exact retry environment/instance labels mismatch')
    elif inventory.stdout.strip():
        raise SystemExit('managed sandboxes already exist; isolated DB cannot reconcile this host')
    env = dict(line.split('=', 1) for line in (root / 'synthetic.env').read_text().splitlines())
    if (len(env.get('ENVIRONMENT_MAINTENANCE_TOKEN', '').encode()) < 32
            or env['ENVIRONMENT_MAINTENANCE_TOKEN'] == env['INTERNAL_API_TOKEN']):
        raise SystemExit('independent maintenance authority required')
    helpers = Path(__file__).parent
    evidence = args.resume_cross_evidence.resolve() if resume else root / ('evidence-' + uuid.uuid4().hex)
    if not resume:evidence.mkdir(mode=0o700)
    command_log=evidence/('commands-retry-'+uuid.uuid4().hex+'.jsonl' if resume else 'commands.jsonl')
    command_sequence=0

    def command(argv):
        nonlocal command_sequence
        result = subprocess.run(argv, capture_output=True, text=True, timeout=180)
        command_sequence+=1
        append_command_evidence(command_log,command_sequence,argv,result)
        return result

    def dc(*parts):
        return command(['docker', 'compose', '-f', str(compose), *parts])

    def runtime_state():
        identity=dc('ps','-q','runtime')
        if identity.returncode or not identity.stdout.strip():
            raise RuntimeError('long Run Runtime identity unavailable')
        result=command(['docker','inspect',identity.stdout.strip(),'--format','{{json .State}}'])
        if result.returncode:raise RuntimeError('long Run Runtime state unavailable')
        return json.loads(result.stdout)

    def admission(marker, operation, session='new'):
        result_path = evidence / (operation + '.json')
        if resume and result_path.exists():
            cached=json.loads(result_path.read_text())
            if cached.get('status')!='accepted' or cached.get('sessionId')!=resume['stateA']['sessionId'] or not cached.get('agentRunId'):
                raise RuntimeError('cached admission binding mismatch')
            return cached
        result = command([sys.executable, str(helpers / 'admit.py'), '--base',
            f"http://127.0.0.1:{manifest['apiPort']}", '--seed', str(root / 'seed.json'),
            '--text', marker, '--operation-id', operation, '--session', session,
            '--result', str(result_path)])
        if result.returncode:
            raise RuntimeError('real admission failed; inspect private evidence')
        return json.loads(result_path.read_text())

    def observed(admitted, name, previous=None):
        path = evidence / (name + '.json')
        argv = [sys.executable, str(helpers / 'observe.py'), '--compose', str(compose),
                '--run', admitted['agentRunId'], '--result', str(path)]
        if previous:
            argv += ['--previous', str(previous)]
        if not args.long_run and not boundary_config:
            def status():
                result=command([sys.executable,str(helpers/'run_status.py'),'--compose',str(compose),
                    '--run',admitted['agentRunId']])
                if result.returncode:raise RuntimeError('Run status unavailable; fixture preserved')
                value=json.loads(next(line for line in reversed(result.stdout.splitlines()) if line.startswith('{')))
                if value.get('schema')!='retained.acceptance.run.status.v1':
                    raise RuntimeError('Run status schema rejected')
                if resume and (value.get('environmentId')!=resume['stateA']['environmentId']
                        or value.get('instanceId') not in (None,resume['stateA']['instanceId'])):
                    raise RuntimeError('retry Run changed environment/instance')
                return value
            def complete():
                result=command(argv)
                if result.returncode:raise RuntimeError('completed Run failed strict physical observation; inspect command evidence')
                return json.loads(path.read_text()),path
            return wait_for_release(admitted,status,complete)
        deadline = time.monotonic() + (7200 if args.long_run else (args.boundary_timeout_seconds+60 if boundary_config else 120))
        while True:
            result = command(argv)
            if not result.returncode:
                return json.loads(path.read_text()), path
            if time.monotonic() >= deadline:
                raise RuntimeError('Run release/physical observation failed within its bounded deadline')
            time.sleep(1)

    progress_evidence=ProgressEvidence()
    long_restart=None
    def wait_long_progress(admitted, previous_path):
        nonlocal long_restart
        deadline=time.monotonic()+7200
        while time.monotonic()<deadline:
            sample_path=evidence/'long-live.json'
            progress_command=[sys.executable,str(helpers/'progress.py'),'--compose',str(compose),
                '--run',admitted['agentRunId'],'--previous',str(previous_path),'--result',str(sample_path)]
            if long_restart is not None:
                progress_command.extend(['--compaction-proof',str(evidence/'long-compaction-before-restart.json')])
            result=command(progress_command)
            if result.returncode:
                if not retryable_observation(result):
                    raise RuntimeError('fatal live observation; preserve fixture and exact failed command')
                time.sleep(1)
                continue
            sample=json.loads(sample_path.read_text())
            progress_evidence.record(sample)
            if progress_evidence.count%32==0 or sample['runStatus']=='completed':
                (evidence/'long-live-samples.json').write_text(json.dumps(progress_evidence.snapshot(),indent=2))
            if should_restart_long_run(sample,long_restart):
                if sample['runStatus']=='completed':
                    raise RuntimeError('long Run completed before a real mid-Run restart could be tested')
                before=runtime_state()
                compaction_before={'agentRunId':admitted['agentRunId'],'sessionId':admitted['sessionId'],
                    'sourceRevision':manifest['sourceRevision'],'binarySha256':manifest['binarySha256'],
                    'completedCommands':sample['completedCommands'],'highestSessionSequence':sample['highestSessionSequence'],
                    'fence':sample['fence'],'durableCompaction':sample['durableCompaction'],'runtimeBefore':before}
                (evidence/'long-compaction-before-restart.json').write_text(json.dumps(compaction_before,indent=2))
                if not before.get('Running') or before.get('OOMKilled') or before.get('ExitCode')==137:
                    raise RuntimeError('Runtime was not healthy before the planned summary recovery restart')
                if dc('restart','runtime').returncode:raise RuntimeError('mid-Run Runtime restart failed')
                after=runtime_state()
                if (not after['Running'] or before['Pid']==after['Pid']
                        or before['StartedAt']==after['StartedAt']):
                    raise RuntimeError('mid-Run Runtime process did not actually restart')
                long_restart={'before':before,'after':after,'completedCommandsBefore':sample['completedCommands'],
                    'beforeFence':sample['fence'],'controllerReclaim':None,'compactionBefore':compaction_before,'compactionRecovery':None,'postRestartBaseline':None}
                (evidence/'long-runtime-restart.json').write_text(json.dumps(long_restart,indent=2))
            if (long_restart is not None and long_restart['controllerReclaim'] is None
                    and sample['runStatus']=='running'
                    and sample['fence']['controllerPurpose']=='execute'
                    and sample['fence']['controllerEpoch']>long_restart['beforeFence']['controllerEpoch']):
                if long_restart['postRestartBaseline'] is None:
                    now=runtime_state()
                    if (not now.get('Running') or now.get('OOMKilled')
                            or any(now[key]!=long_restart['after'][key] for key in ('StartedAt','Pid'))):
                        raise RuntimeError('Runtime changed before the post-restart evidence baseline')
                    long_restart['postRestartBaseline']={'highestSessionSequence':sample['highestSessionSequence'],
                        'completedCommands':sample['completedCommands'],'fence':sample['fence'],'runtime':now}
                    (evidence/'long-compaction-post-restart-baseline.json').write_text(json.dumps(long_restart['postRestartBaseline'],indent=2))
                    (evidence/'long-runtime-restart.json').write_text(json.dumps(long_restart,indent=2))
                recovery=compaction_recovery_proof(long_restart['compactionBefore'],sample,long_restart['postRestartBaseline'])
                if recovery is not None:
                    long_restart['controllerReclaim']=probe_reclaimed_controller(sample['containerId'],
                        long_restart['beforeFence'],sample['fence'],sample['live']['backgroundTaskIds'][0])
                    long_restart['compactionRecovery']=recovery
                    (evidence/'long-runtime-restart.json').write_text(json.dumps(long_restart,indent=2))
            if sample['runStatus']=='completed':
                if long_restart is None:
                    raise RuntimeError('long Run lacks complete work: no committed and observed compaction summary before restart')
                if (sample['completedCommands']!=1000 or long_restart is None
                        or long_restart['controllerReclaim'] is None or long_restart['compactionRecovery'] is None
                        or progress_evidence.maxima.get('store.indexedRunTasks',0)<=1000):
                    raise RuntimeError('long Run lacks complete work, committed summary reuse, and mid-Run restart evidence')
                return
            time.sleep(1)
        if long_restart is None:
            raise RuntimeError('long Run has no committed and observed summary for restart within7200s; fixture preserved')
        if long_restart['compactionRecovery'] is None:
            raise RuntimeError('long Run did not reuse its committed summary after restart within7200s; fixture preserved')
        raise RuntimeError('long Run progress exceeded7200s; fixture preserved')

    if resume:
        a=resume['a'];state_a=resume['stateA'];path_a=evidence/'observed-a.json'
    else:
        a = admission('ACCEPTANCE_RUN_A', 'acceptance-a-' + uuid.uuid4().hex)
        state_a, path_a = observed(a, 'observed-a')
        if dc('restart', 'runtime').returncode:
            raise RuntimeError('owned Runtime restart failed')
    candidate_image=command(['docker','image','inspect','--format','{{.Id}}',project+'-general:fixed'])
    if candidate_image.returncode:
        raise RuntimeError('candidate tools image identity unavailable')
    b = resume['b'] if resume else admission(boundary_marker or ('ACCEPTANCE_LONG_RUN' if args.long_run else 'ACCEPTANCE_RUN_B'), 'acceptance-b-' + uuid.uuid4().hex, a['sessionId'])
    boundary_proof=None
    if args.long_run:
        wait_long_progress(b,path_a)
    elif boundary_config:
        def restart_boundary():
            if dc('restart','runtime').returncode:raise RuntimeError('boundary Runtime restart failed')
        def release_boundary(container,config):
            value=boundary.release_token(config)
            path=boundary.paths(config)[1]
            # Unique validated hex nonce/call/index only. Noclobber refuses an
            # existing release file; this is owned fixture control, not a tool.
            result=command(['docker','exec','--user','0:0',container,'/bin/sh','-c',
                "set -C; printf '%s' "+value+' > '+path])
            if result.returncode:raise RuntimeError('boundary release failed; do not automatically retry')
        def save_boundary(name,value):
            with (evidence/name).open('x',encoding='utf-8') as stream:json.dump(value,stream,indent=2)
        boundary_proof=boundary.execute_restart(b,boundary_config,state_a,
            lambda:boundary.read_state(compose,b,boundary_config,command),runtime_state,restart_boundary,
            task_rpc,release_boundary,save_boundary,probe_reclaimed_controller)
    state=resume['stateB'] if resume else observed(b, 'observed-b', path_a)[0]
    completed_ordinary=state_a['successfulOrdinaryCommands']+state['successfulOrdinaryCommands']
    history=None
    for index in range(0 if args.long_run or boundary_config else 98):
        marker = 'ACCEPTANCE_HISTORY' if index == 0 else 'ACCEPTANCE_RUN_B'
        if resume and index==0:
            admitted=resume['history']
        else:
            operation='acceptance-use-'+str(index)+'-'+(evidence.name.removeprefix('evidence-') if resume else uuid.uuid4().hex)
            admitted=admission(marker,operation,a['sessionId'])
        state, _ = observed(admitted, 'observed-use-' + str(index), path_a)
        completed_ordinary+=state['successfulOrdinaryCommands']
        if index==0:
            history=history_metrics(state)
    marker_proof=None
    automatic_instructions=None
    uses=2 if args.long_run or boundary_config else 100
    if args.long_run:
        facts=state.get('workloadObservation')
        if (not isinstance(facts,dict) or {key:facts.get(key) for key in ('attemptedCommands','completedCommands','distinctCallIds')}
                !={'attemptedCommands':1000,'completedCommands':1000,'distinctCallIds':1000}
                or facts.get('agentsInstructions',{}).get('mainRequestsWithAgentsInstructions',0)<1000
                or facts['agentsInstructions'].get('agentsFileSha256')!=state['agentsFileSha256']):
            raise RuntimeError('long Run lacks1000 actual distinct successful command results')
        logs=dc('logs','--no-color','--no-log-prefix','model')
        if logs.returncode:raise RuntimeError('actual model-request metadata unavailable')
        automatic_instructions=instructions_facts(logs.stdout.splitlines(),state['agentsFileSha256'])
        (evidence/'automatic-instructions.json').write_text(json.dumps(automatic_instructions,indent=2))
        markers=command(['docker','exec',state['containerId'],'head','-c','65537',
            '/mnt/data/retained-long-run-markers.txt'])
        if markers.returncode or markers.stdout.splitlines()!=['%04d'%i for i in range(1000)]:
            raise RuntimeError('actual long-command side effects are missing, duplicated or reordered')
        marker_proof={'lines':1000,'bytes':len(markers.stdout.encode()),
            'sha256':hashlib.sha256(markers.stdout.encode()).hexdigest(),'exactSequence':True}
        (evidence/'long-side-effect-proof.json').write_text(json.dumps(marker_proof,indent=2))
    elif boundary_config:
        facts=boundary.read_state(compose,b,boundary_config,command)
        if facts is None or facts['runStatus']!='completed':raise RuntimeError('boundary Run did not complete')
        boundary.validate_facts(facts,boundary_config,b,complete=True)
        markers=command(['docker','exec',state['containerId'],'head','-c','4097',boundary.paths(boundary_config)[0]])
        if markers.returncode or markers.stdout.splitlines()!=['%04d'%i for i in range(boundary_config['steps'])]:
            raise RuntimeError('boundary side effects missing/duplicated/reordered')
        boundary_proof['completedSteps']=boundary_config['steps']
        boundary_proof['originalResult']=next(result for result in facts['results'] if result['callId']==boundary_proof['callId'])
        boundary_proof['sideEffects']={'lines':boundary_config['steps'],'sha256':hashlib.sha256(markers.stdout.encode()).hexdigest(),'exactSequence':True}
        (evidence/'boundary-completion-proof.json').write_text(json.dumps(boundary_proof,indent=2))
    elif history is None or completed_ordinary <= 64:
        raise RuntimeError('history pressure or cross-window command evidence missing')

    request = {'schema':'runtime.environment.retire.v1', 'operationId':'retire-' + uuid.uuid4().hex,
        'environmentId':state['environmentId'], 'instanceId':state['instanceId'],
        'hostId':state['hostId'], 'expectedControllerEpoch':state['fence']['controllerEpoch'],
        'acknowledgeTmpfsLoss':True}
    (evidence / 'retirement-request.json').write_text(json.dumps(request, indent=2))
    def retire(discard=False):
        req = urllib.request.Request(
            f"http://127.0.0.1:{manifest['runtimePort']}/internal/environments/retire",
            data=json.dumps(request).encode(), headers={'Content-Type':'application/json',
                'x-internal-token':env['INTERNAL_API_TOKEN'],
                'x-environment-maintenance-token':env['ENVIRONMENT_MAINTENANCE_TOKEN']})
        with urllib.request.urlopen(req, timeout=60) as response:
            if discard:
                return None  # Deliberate caller ACK discard; same stable request reconciles.
            result = json.load(response)
        if (result.get('schema') != 'runtime.environment.retire.result.v1'
                or any(result.get(key) != request[key] for key in ('operationId','environmentId','instanceId'))):
            raise RuntimeError('maintenance response identity mismatch')
        return result
    retire(discard=True)
    result = retire()
    (evidence / 'retirement-retry.json').write_text(json.dumps(result, indent=2))
    if result.get('status') != 'destroyed':
        raise RuntimeError('maintenance remains pending; preserve fixture and resource holding')
    inventory = command(['docker', 'ps', '-aq', '--no-trunc'])
    if inventory.returncode or state['containerId'] in inventory.stdout.splitlines():
        raise RuntimeError('physical container absence was not established')
    query = """
import json
from django.db import connection
with connection.cursor() as c:
 c.execute('SELECT state,environment_id,instance_id,host_id FROM runtime.environment_retirements WHERE operation_id=%s',[OP])
 receipt=c.fetchall()
 assert receipt==[('destroyed',ENV,INSTANCE,HOST)],'durable maintenance receipt mismatch'
 c.execute('SELECT count(*) FROM runtime.resident_sandboxes WHERE environment_id=%s',[ENV])
 assert c.fetchone()[0]==0,'resident resource holding not released'
 c.execute('SELECT count(*) FROM runtime.execution_instances WHERE environment_id=%s AND instance_id=%s',[ENV,INSTANCE])
 assert c.fetchone()[0]>0,'allocation history lost'
 c.execute('SELECT count(*) FROM app_core_agentrunenvironmentrelease r JOIN app_core_agentrun a ON a.id=r.agent_run_id WHERE a.environment_id=%s',[ENV])
 assert c.fetchone()[0]==USES,'release count must match all actual uses'
print(json.dumps({'maintenanceReceiptCount':len(receipt),'status':receipt[0][0],'residentHoldingCount':0,'historyPreserved':True}))
"""
    for key, value in [('OP',request['operationId']),('ENV',state['environmentId']),
                       ('INSTANCE',state['instanceId']),('HOST',state['hostId']),('USES',uses)]:
        query = query.replace(key, repr(value))
    verified = dc('exec', '-T', 'api', '/app/.venv/bin/python', 'manage.py', 'shell', '-c', query)
    if verified.returncode:
        raise RuntimeError('resource receipt verification failed; preserve evidence')
    result = retire()
    if result.get('status') != 'destroyed':
        raise RuntimeError('completed maintenance replay changed result')
    (evidence / 'summary.json').write_text(json.dumps({'sourceRevision':manifest['sourceRevision'],
        'binarySha256':manifest['binarySha256'], 'uses':uses,'physicalChain':'passed',
        'driverSha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        'resumedCrossRun':bool(resume),'retryCommandEvidence':command_log.name if resume else None,
        'sourceTree':manifest['buildManifest']['sourceTree'],'baseImages':manifest['images'],
        'candidateToolsImageId':candidate_image.stdout.strip(),
        'maintenanceResponseDiscarded':True,'containerAbsent':True,'remoteCI':'not-evaluated',
        'completedRuns':uses,'failedRuns':0,'historyObservation':history,
        'longRunObservation':state.get('workloadObservation') if args.long_run else None,
        'boundaryRunObservation':boundary_proof,
        'longRunObservationCount':progress_evidence.count,'longRunRetainedSamples':len(progress_evidence.samples),'longRunRuntimeRestart':long_restart,
        'longRunMaxIndexedRunTasks':progress_evidence.maxima.get('store.indexedRunTasks',0),
        'longRunMetricMaxima':progress_evidence.maxima,
        'longRunExtraIndexedTasksAreUncategorized':True if args.long_run else None,
        'automaticInstructionsObservation':automatic_instructions,'longSideEffectProof':marker_proof,
        'completedOrdinaryCommandsAcrossRuns':completed_ordinary,
        'unrun':['more-than256-inputs','near-journal-capacity','concurrent-run-retirement',
            'revoked-user-maintenance','unobservable-docker','descendant-task-isolation'],
        'evidenceDirectory':str(evidence)}, indent=2))
    print(json.dumps({'status':'passed-limited-physical-sequence','evidenceDirectory':str(evidence)}))


if __name__ == '__main__':
    main()
