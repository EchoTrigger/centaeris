"""Exercise an already prepared owned fixture; no global cleanup or auto-teardown.

The caller authorizes destruction only of this generated fixture's exact retained
instance. Private credentials are read locally and never included in evidence.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time
import urllib.request
import uuid


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--fixture', required=True, type=Path)
    parser.add_argument('--acknowledge-tmpfs-loss', action='store_true', required=True)
    args = parser.parse_args()
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
    inventory = subprocess.run(['docker','ps','-aq','--filter','label=centaeris.managed=true'],
                               capture_output=True,text=True,check=True)
    if inventory.stdout.strip():
        raise SystemExit('managed sandboxes already exist; isolated DB cannot reconcile this host')
    env = dict(line.split('=', 1) for line in (root / 'synthetic.env').read_text().splitlines())
    if (len(env.get('ENVIRONMENT_MAINTENANCE_TOKEN', '').encode()) < 32
            or env['ENVIRONMENT_MAINTENANCE_TOKEN'] == env['INTERNAL_API_TOKEN']):
        raise SystemExit('independent maintenance authority required')
    helpers = Path(__file__).parent
    evidence = root / ('evidence-' + uuid.uuid4().hex)
    evidence.mkdir(mode=0o700)
    transcript = []

    def command(argv):
        result = subprocess.run(argv, capture_output=True, text=True, timeout=180)
        transcript.append({'command':argv, 'exitCode':result.returncode,
                           'stdout':result.stdout, 'stderr':result.stderr})
        (evidence / 'commands.json').write_text(json.dumps(transcript, indent=2))
        return result

    def dc(*parts):
        return command(['docker', 'compose', '-f', str(compose), *parts])

    def admission(marker, operation, session='new'):
        result_path = evidence / (operation + '.json')
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
        deadline = time.monotonic() + 120
        while True:
            result = command(argv)
            if not result.returncode:
                return json.loads(path.read_text()), path
            if time.monotonic() >= deadline:
                raise RuntimeError('Run release/physical observation failed within120s')
            time.sleep(1)

    a = admission('ACCEPTANCE_RUN_A', 'acceptance-a-' + uuid.uuid4().hex)
    state_a, path_a = observed(a, 'observed-a')
    if dc('restart', 'runtime').returncode:
        raise RuntimeError('owned Runtime restart failed')
    candidate_image=command(['docker','image','inspect','--format','{{.Id}}',project+'-general:fixed'])
    if candidate_image.returncode:
        raise RuntimeError('candidate tools image identity unavailable')
    b = admission('ACCEPTANCE_RUN_B', 'acceptance-b-' + uuid.uuid4().hex, a['sessionId'])
    state, _ = observed(b, 'observed-b', path_a)
    for index in range(98):
        marker = 'ACCEPTANCE_HISTORY' if index == 0 else 'ACCEPTANCE_RUN_B'
        admitted = admission(marker, 'acceptance-use-' + str(index) + '-' + uuid.uuid4().hex,
                             a['sessionId'])
        state, _ = observed(admitted, 'observed-use-' + str(index), path_a)
    model_logs=dc('logs','--no-log-prefix','model')
    history=[]
    for line in model_logs.stdout.splitlines():
        try:
            item=json.loads(line)
        except ValueError:
            continue
        if item.get('kind')=='historyObservation':
            history.append(item)
    if (model_logs.returncode or len(history)!=1
            or not 1<=history[0]['completedCommands']<=64
            or not history[0]['explicitRecoverableCapacity']
            or history[0]['attemptedCommands']<=history[0]['completedCommands']):
        raise RuntimeError('same-Run history did not provide successful commands and explicit recoverable capacity')

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
 assert c.fetchone()[0]>=100,'release history lost'
print(json.dumps({'maintenanceReceiptCount':len(receipt),'status':receipt[0][0],'residentHoldingCount':0,'historyPreserved':True}))
"""
    for key, value in [('OP',request['operationId']),('ENV',state['environmentId']),
                       ('INSTANCE',state['instanceId']),('HOST',state['hostId'])]:
        query = query.replace(key, repr(value))
    verified = dc('exec', '-T', 'api', '/app/.venv/bin/python', 'manage.py', 'shell', '-c', query)
    if verified.returncode:
        raise RuntimeError('resource receipt verification failed; preserve evidence')
    result = retire()
    if result.get('status') != 'destroyed':
        raise RuntimeError('completed maintenance replay changed result')
    (evidence / 'summary.json').write_text(json.dumps({'sourceRevision':manifest['sourceRevision'],
        'binarySha256':manifest['binarySha256'], 'uses':100,'physicalChain':'passed',
        'sourceTree':manifest['buildManifest']['sourceTree'],'baseImages':manifest['images'],
        'candidateToolsImageId':candidate_image.stdout.strip(),
        'maintenanceResponseDiscarded':True,'containerAbsent':True,'remoteCI':'not-evaluated',
        'historyObservation':history[0],'nextRunRecoveredAfterHistory':True,
        'completedOrdinaryCommandsAcrossRuns':98+history[0]['completedCommands'],
        'unrun':['more-than256-inputs','near-journal-capacity','concurrent-run-retirement',
            'revoked-user-maintenance','unobservable-docker','descendant-task-isolation'],
        'evidenceDirectory':str(evidence)}, indent=2))
    print(json.dumps({'status':'passed-limited-physical-sequence','evidenceDirectory':str(evidence)}))


if __name__ == '__main__':
    main()
