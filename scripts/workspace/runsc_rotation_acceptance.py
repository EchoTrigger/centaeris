"""Product-chain gate for retained-memory runsc pause/resume rotation.

Uses the existing owned retained fixture, public admission and read-only status
observation. Secrets stay in fixture files and never enter command arguments or
result JSON. No global cleanup or daemon configuration is performed.
"""
import argparse
import hashlib
import http.cookiejar
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import time
import urllib.request
import uuid

SCHEMA = 'runsc.rotation.acceptance.v1'
ORACLE_SCHEMA = 'runsc.rotation.oracle.v1'
MARKER = re.compile(r'RUNSC_ROTATION ([a-f0-9]{32}) ([AB]) (\d{1,2})\Z')
BLOCK = re.compile(r'RUNSC_ROTATION_BLOCK ([a-f0-9]{32}) ([AB])\Z')


def challenge_proof(nonce, challenge):
    return hashlib.sha256((nonce + ':' + challenge).encode()).hexdigest()


def oracle_root(tag, label):
    if not re.fullmatch('[a-f0-9]{32}', tag) or label not in ('A', 'B'):
        raise ValueError('invalid oracle identity')
    return '/mnt/data/runsc-rotation-' + tag + '-' + label


def memory_oracle_server(tag, label):
    """Return argv for one persistent process; its random nonce is never saved."""
    root = oracle_root(tag, label)
    code = r'''import hashlib,json,os,secrets,socket,sys
root=sys.argv[1]
nonce=secrets.token_hex(32)
pid=os.getpid()
start=int(open('/proc/self/stat').read().split()[21])
with open(root+'.txt','x') as f:f.write('retained rotation '+root+'\n')
file_hash=hashlib.sha256(open(root+'.txt','rb').read()).hexdigest()
server=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)
server.bind(root+'.sock')
os.chmod(root+'.sock',0o600)
server.listen(2)
counter=0
while True:
 client,_=server.accept()
 with client:
  client.settimeout(5)
  data=client.recv(1025)
  if len(data)>1024:continue
  challenge=data.decode().strip()
  counter+=1
  file_hash=hashlib.sha256(open(root+'.txt','rb').read()).hexdigest()
  result={'schema':'runsc.rotation.oracle.v1','nonce':nonce,'pid':pid,'startTicks':start,'counter':counter,'fileSha256':file_hash,'challenge':challenge,'proof':hashlib.sha256((nonce+':'+challenge).encode()).hexdigest()}
  client.sendall((json.dumps(result)+'\n').encode())
'''
    return ['python3', '-u', '-c', code, root]


def memory_oracle_client(tag, label, index):
    root = oracle_root(tag, label)
    challenge = f'{tag}-{label}-{index}'
    code = r'''import json,socket,sys,time
path,challenge=sys.argv[1:]
end=time.monotonic()+15
while True:
 try:
  s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);s.settimeout(3);s.connect(path);break
 except (OSError,TimeoutError):
  s.close()
  if time.monotonic()>=end:raise
  time.sleep(.05)
with s:
 s.sendall((challenge+'\n').encode());data=b''
 while not data.endswith(b'\n') and len(data)<=4096:
  part=s.recv(4096)
  if not part:break
  data+=part
 value=json.loads(data)
 if value['challenge']!=challenge:raise RuntimeError('oracle challenge mismatch')
 print('RUNSC_ROTATION_ORACLE '+json.dumps(value))
'''
    return shlex.join(['python3', '-c', code, root + '.sock', challenge])


def _text(message):
    value = message.get('content', '')
    if isinstance(value, str):
        return value
    if isinstance(value, list):
        return '\n'.join(item.get('text', '') for item in value if isinstance(item, dict))
    return ''


def extract_oracle(text):
    decoder = json.JSONDecoder()
    start = text.find('RUNSC_ROTATION_ORACLE ')
    if start < 0:
        raise RuntimeError('actual task result lacks memory oracle output')
    value, _ = decoder.raw_decode(text[start + len('RUNSC_ROTATION_ORACLE '):])
    if not isinstance(value, dict) or value.get('schema') != ORACLE_SCHEMA:
        raise RuntimeError('memory oracle response schema invalid')
    return value


def next_rotation_call(request, progress=None):
    messages = request['messages']
    last = max(i for i, item in enumerate(messages) if item.get('role') == 'user')
    marker = _text(messages[last])
    replies = [item for item in messages[last + 1:] if item.get('role') == 'tool']
    block = BLOCK.fullmatch(marker)
    if block:
        return ('bash', {'command': 'sleep 120', 'timeout_ms': 180000}) if not replies else None
    match = MARKER.fullmatch(marker)
    if match is None:
        raise ValueError('unsupported rotation prompt')
    tag, label, index = match.groups()
    index = int(index)
    if index >= 12 or label != ('A' if index % 2 == 0 else 'B'):
        raise ValueError('rotation round identity invalid')
    setup = index < 2
    if setup and not replies:
        argv = memory_oracle_server(tag, label)
        return 'background_task_start', {'program': argv[0], 'args': argv[1:], 'cwd': '/mnt/data', 'timeout_ms': 14700000}
    if len(replies) == int(setup):
        command = memory_oracle_client(tag, label, index)
        if index == 4:
            command = 'sleep 15; ' + command
        return 'bash', {'command': command, 'timeout_ms': 30000}
    if len(replies) != 1 + int(setup):
        raise ValueError('unexpected rotation tool count')
    oracle = extract_oracle(_text(replies[-1]))
    if oracle.get('challenge') != f'{tag}-{label}-{index}':
        raise ValueError('rotation result belongs to another command')
    return None


def validate_evidence(evidence):
    try:
        if evidence['schema'] != SCHEMA or evidence['runningLimit'] != 1:
            raise RuntimeError('rotation evidence schema or running limit invalid')
        if not re.fullmatch('[a-f0-9]{40}', evidence['sourceRevision']):
            raise RuntimeError('exact source revision missing')
        runs = evidence['runs']
        if len(runs) != 12 or len({row['agentRunId'] for row in runs}) != 12:
            raise RuntimeError('twelve distinct actual Runs required')
        baselines = {}
        challenges = set()
        previous_counter = {}
        memory_total = None
        for index, row in enumerate(runs):
            label = 'A' if index % 2 == 0 else 'B'
            if row['label'] != label or row['runStatus'] != 'completed' or row['coreTerminalType'] != 'agent_run_completed':
                raise RuntimeError('alternating Run did not complete')
            if row['releaseReceiptCount'] != 1 or row['cleanupComplete'] is not True:
                raise RuntimeError('Run cleanup/release obligation incomplete')
            if (row['runtime'] != 'runsc' or row['running'] is not True or row['paused'] is not True or row['powerState'] != 'paused'
                    or row['runningReservations'] != 0):
                raise RuntimeError('idle pause lacks matching physical and durable acknowledgement')
            if row['residentCount'] != (1 if index == 0 else 2) or row['residentMemoryBytes'] <= 0:
                raise RuntimeError('paused memory/resident responsibility was released')
            if index == 1:
                memory_total = row['residentMemoryBytes']
                if memory_total <= runs[0]['residentMemoryBytes']:
                    raise RuntimeError('second environment has no resident memory charge')
            elif index > 1 and row['residentMemoryBytes'] != memory_total:
                raise RuntimeError('resident memory charge changed during rotation')
            pids=row['pids']
            if (row['residentPids']<=0 or pids['max']!=row['residentPids'] or not 0<pids['current']<=pids['max'] or pids['eventsMax']!=0):
                raise RuntimeError('PID charge/controller exhausted or changed')
            oracle = row['oracle']
            if (oracle['schema'] != ORACLE_SCHEMA or not re.fullmatch('[a-f0-9]{64}', oracle['nonce'])
                    or type(oracle['pid']) is not int or oracle['pid'] <= 0
                    or type(oracle['startTicks']) is not int or oracle['startTicks'] <= 0
                    or type(oracle['counter']) is not int or oracle['counter'] <= previous_counter.get(label, 0)
                    or not re.fullmatch('[a-f0-9]{64}', oracle['fileSha256'])
                    or oracle['proof'] != challenge_proof(oracle['nonce'], oracle['challenge'])
                    or oracle['challenge'] in challenges):
                raise RuntimeError('fresh in-memory process challenge was not preserved')
            challenges.add(oracle['challenge'])
            previous_counter[label] = oracle['counter']
            identity = tuple(row[key] for key in ('sessionId', 'environmentId', 'instanceId', 'containerId'))
            identity += tuple(oracle[key] for key in ('nonce', 'pid', 'startTicks', 'fileSha256'))
            if label in baselines and baselines[label] != identity:
                raise RuntimeError('resume replaced environment, process memory or file')
            baselines[label] = identity
        if baselines['A'][1:4] == baselines['B'][1:4] or baselines['A'][4] == baselines['B'][4]:
            raise RuntimeError('two independent environments required')
        wakeup = evidence['wakeup']
        if (wakeup['waitingRunId'] != runs[5]['agentRunId'] or wakeup['waitObserved'] is not True
                or wakeup['waitingPaused'] is not True or wakeup['runningReservations'] != 1
                or wakeup['completedWithoutReadmission'] is not True):
            raise RuntimeError('capacity release did not wake the original queued demand')
        restart = evidence['restart']
        if (restart['performed'] is not True or restart['beforeRuntimeStartedAt'] == restart['afterRuntimeStartedAt']
                or set(restart['preservedContainers']) != {runs[0]['containerId'], runs[1]['containerId']}):
            raise RuntimeError('Runtime restart did not preserve both exact containers')
        cancellation = evidence['cancellation']
        if (cancellation['queuedRunStatus'] != 'cancelled' or cancellation['holderRunStatus'] != 'cancelled'
                or cancellation['queuedSuccessfulCommands'] != 0 or cancellation['runningReservationsAfter'] != 0):
            raise RuntimeError('queued cancellation did not release demand safely')
        retired = evidence['retirements']
        if len(retired) != 2 or {r['environmentId'] for r in retired} != {runs[0]['environmentId'], runs[1]['environmentId']}:
            raise RuntimeError('both exact environments must retire')
        if any(r['status'] != 'destroyed' or r['containerAbsent'] is not True or r['residentHoldingCount'] != 0
               or r['historyPreserved'] is not True for r in retired):
            raise RuntimeError('retirement physical absence or durable release not established')
        return {'schema': SCHEMA, 'status': 'passed', 'sourceRevision': evidence['sourceRevision'],
                'completedRuns': 12, 'environments': 2, 'runningLimit': 1,
                'memoryRetainedWhilePaused': True, 'hostMemoryReleased': False,
                'restart': 'passed', 'queuedCancellation': 'passed', 'capacityWake': 'passed', 'retirement': 'passed'}
    except (KeyError, TypeError, ValueError) as error:
        raise RuntimeError('rotation evidence missing required facts') from error


class FixtureDriver:
    def __init__(self, fixture, result_directory):
        self.root = fixture.resolve(strict=True)
        self.manifest = json.loads((self.root / 'manifest.json').read_text())
        self.compose = self.root / 'compose.json'
        self.result = result_directory
        self.result.mkdir(mode=0o700, parents=False, exist_ok=False)
        self.tag = uuid.uuid4().hex
        self.helpers = Path(__file__).with_name('runsc_acceptance')
        config = json.loads(self.compose.read_text())
        project = self.manifest['project']
        if not re.fullmatch(r'retainedaccept[a-f0-9]{10}', project) or config['name'] != project:
            raise RuntimeError('owned retained fixture identity required')
        from runsc_acceptance.isolation import verify_owner
        verify_owner(Path(self.manifest['isolationOwner']))
        self.private = dict(line.split('=', 1) for line in (self.root / 'synthetic.env').read_text().splitlines() if line)
        runtime_env = {**self.private, **config['services']['runtime'].get('environment', {})}
        if (runtime_env.get('OCI_RUNTIME') != 'runsc'
                or runtime_env.get('RUNTIME_RUNSC_MAX_RUNNING_ENVIRONMENTS') != '1'
                or int(runtime_env.get('RUNTIME_RUNSC_IDLE_PAUSE_MS', '0')) <= 0):
            raise RuntimeError('fixture must explicitly select runsc, Docker, one running slot and automatic pause')
        socket_mounts = [value.split(':/var/run/docker.sock')[0] for value in config['services']['runtime']['volumes']
                         if isinstance(value, str) and ':/var/run/docker.sock' in value]
        if len(socket_mounts) != 1 or not socket_mounts[0].startswith('/'):
            raise RuntimeError('one explicit fixture Docker socket required')
        socket = socket_mounts[0]
        if socket == '/var/run/docker.sock':
            raise RuntimeError('dedicated disposable Docker daemon required; default daemon is rejected')
        self.environment = {**os.environ, 'DOCKER_HOST': 'unix://' + socket}
        self.docker = ['docker', '--host', 'unix://' + socket]
        managed = self.command(self.docker + ['ps', '-aq', '--filter', 'label=centaeris.managed=true'])
        if managed.strip():
            raise RuntimeError('managed sandboxes already exist on disposable daemon; preserve them')
        source = Path(self.manifest['source']).resolve(strict=True)
        if source != Path(__file__).resolve().parents[2]:
            raise RuntimeError('fixture source differs from acceptance driver checkout')
        if self.command(['git', '-C', str(source), 'rev-parse', 'HEAD']).strip() != self.manifest['sourceRevision']:
            raise RuntimeError('fixture source revision changed')
        if self.command(['git', '-C', str(source), 'status', '--porcelain']).strip():
            raise RuntimeError('fixture requires a clean committed candidate')
        binaries = Path(self.manifest['binaryDirectory'])
        if any(hashlib.sha256((binaries / name).read_bytes()).hexdigest() != digest
               for name, digest in self.manifest['binarySha256'].items()):
            raise RuntimeError('fixture binaries changed since preparation')
        self.sessions = {}
        self.last = {}
        self.seed = json.loads((self.root / 'seed.json').read_text())
        self.base = f"http://127.0.0.1:{self.manifest['apiPort']}"
        jar = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
        self.csrf = self.http('/api/csrf')['csrfToken']
        self.http('/api/login', {'email': self.seed['email'], 'password': self.seed['password']})
        self.csrf = self.http('/api/csrf')['csrfToken']

    def command(self, argv, timeout=60):
        value = subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True, text=True,
                               env=self.environment, timeout=timeout, check=True)
        if len(value.stdout.encode()) > 1024 * 1024:
            raise RuntimeError('fixture observation exceeded output bound')
        return value.stdout

    def dc(self, *args):
        return self.command(self.docker + ['compose', '-f', str(self.compose), *args])

    def http(self, path, payload=None):
        headers = {'Origin': self.base, 'Referer': self.base + '/'}
        if payload is not None:
            headers.update({'Content-Type': 'application/json', 'X-CSRFToken': self.csrf})
        request = urllib.request.Request(self.base + path, data=None if payload is None else json.dumps(payload).encode(), headers=headers)
        with self.opener.open(request, timeout=30) as response:
            return json.load(response)

    def admit(self, label, marker):
        target = self.result / ('admission-' + uuid.uuid4().hex + '.json')
        self.command([sys.executable, str(self.helpers / 'admit.py'), '--base', self.base,
                      '--seed', str(self.root / 'seed.json'), '--text', marker,
                      '--session', self.sessions.get(label, 'new'), '--operation-id', 'rotation-' + uuid.uuid4().hex,
                      '--result', str(target)])
        admitted = json.loads(target.read_text())
        self.sessions[label] = admitted['sessionId']
        return admitted

    def sql(self, code):
        raw = self.dc('exec', '-T', 'api', '/app/.venv/bin/python', 'manage.py', 'shell', '-c', code)
        return json.loads(next(line for line in reversed(raw.splitlines()) if line.startswith('{')))

    def status(self, admitted):
        raw = self.command([sys.executable, str(self.helpers / 'run_status.py'), '--compose', str(self.compose),
                            '--run', admitted['agentRunId']])
        return json.loads(raw.splitlines()[-1])

    def wait(self, observe, predicate, description, timeout=120):
        end = time.monotonic() + timeout
        while True:
            value = observe()
            if predicate(value):
                return value
            if time.monotonic() >= end:
                raise RuntimeError('timed out waiting for ' + description)
            time.sleep(.2)

    def completed(self, admitted):
        def ready(value):
            if value['runStatus'] in ('failed', 'cancelled'):
                raise RuntimeError('rotation Run failed instead of completing')
            return (value['runStatus'] == 'completed' and value['coreTerminalType'] == 'agent_run_completed'
                    and value['releaseReceiptCount'] == 1 and value['cleanupComplete'])
        return self.wait(lambda: self.status(admitted), ready, 'completed product Run')

    def inspect(self, container):
        raw = self.command(self.docker + ['inspect', container, '--format',
            '{{json .State}} {{json .Config.Labels}} {{json .HostConfig.Runtime}}'])
        decoder = json.JSONDecoder()
        values = []
        for _ in range(3):
            value, end = decoder.raw_decode(raw.lstrip())
            values.append(value)
            raw = raw.lstrip()[end:]
        return values

    def pid_metrics(self,container,pid,limit):
        from runsc_acceptance.daemon import cgroup_pids
        return cgroup_pids(container,pid,limit)

    def observe(self, label, admitted):
        code = r'''import json
from django.conf import settings
from django.db import connection,transaction
from app_core.models import AgentRun,SessionEvent
assert settings.DATABASES['default']['NAME']=='retained_acceptance'
with transaction.atomic():
 with connection.cursor() as c:
  c.execute('SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY')
  run=AgentRun.objects.select_related('environment').get(pk=RUN)
  c.execute('SELECT container_id,instance_id,host_id,memory_bytes,execution_id,pids FROM runtime.resident_sandboxes WHERE environment_id=%s',[run.environment_id])
  residents=c.fetchall()
  if len(residents)!=1:raise RuntimeError('exact resident binding unavailable')
  container,instance,host,memory,allocation,pids=residents[0]
  c.execute('SELECT state,controller_epoch FROM runtime.runsc_environment_power WHERE environment_id=%s AND host_id=%s AND instance_id=%s AND container_id=%s AND allocation_execution_id=%s',[run.environment_id,host,instance,container,allocation])
  power=c.fetchone()
  c.execute("SELECT count(*),COALESCE(sum(memory_bytes),0) FROM runtime.resident_sandboxes")
  count,total=c.fetchone()
  exact_total=int(total)
  if total!=exact_total:raise RuntimeError('resident memory sum is not an integer')
  total=exact_total
  c.execute("SELECT count(*) FROM runtime.resident_sandboxes r WHERE r.allocation_mode='retained' AND NOT EXISTS (SELECT 1 FROM runtime.runsc_environment_power p WHERE p.environment_id=r.environment_id AND p.host_id=r.host_id AND p.instance_id=r.instance_id AND p.container_id=r.container_id AND p.allocation_execution_id=r.execution_id AND p.state='paused')")
  running=c.fetchone()[0]
  outputs=list(SessionEvent.objects.filter(agent_run=run,session_level=False,payload__type='tool_result').order_by('agent_run_sequence').values_list('payload',flat=True))
  print(json.dumps({'environmentId':run.environment_id,'instanceId':instance,'containerId':container,'hostId':host,'controllerEpoch':run.environment.controller_epoch,'powerState':power[0] if power else None,'runningReservations':running,'residentCount':count,'residentMemoryBytes':total,'residentPids':pids,'successfulCommands':sum(e['payload'].get('toolName')=='bash' and e['payload'].get('resultState') in ('successWithOutput','successNoOutput') for e in outputs),'outputs':[e['payload'].get('modelContent','') for e in outputs]}))
'''.replace('RUN', repr(admitted['agentRunId']))
        row = self.sql(code)
        physical, labels, runtime = self.inspect(row['containerId'])
        if (labels.get('centaeris.managed') != 'true' or labels.get('centaeris.environment_id') != row['environmentId']
                or labels.get('centaeris.instance_id') != row['instanceId']):
            raise RuntimeError('observed container labels do not match durable identity')
        row['pids']=self.pid_metrics(row['containerId'],physical['Pid'],row['residentPids'])
        if row['runningReservations'] > 1:
            raise RuntimeError('one-slot running budget exceeded')
        oracles = [extract_oracle(output) for output in row.pop('outputs') if 'RUNSC_ROTATION_ORACLE ' in output]
        row.update(label=label, agentRunId=admitted['agentRunId'], sessionId=admitted['sessionId'],
                   runtime=runtime, paused=physical['Paused'], running=physical['Running'])
        if oracles:
            if len(oracles) != 1:
                raise RuntimeError('Run has ambiguous memory oracle results')
            row['oracle'] = oracles[0]
        return row

    def paused(self, label, admitted):
        return self.wait(lambda: self.observe(label, admitted),
                         lambda row: row['powerState'] == 'paused' and row['paused'] and row['runningReservations'] == 0,
                         'acknowledged idle pause')

    def runtime_state(self):
        container = self.dc('ps', '-q', 'runtime').strip()
        if not container:
            raise RuntimeError('owned fixture Runtime unavailable')
        state, labels, _ = self.inspect(container)
        if labels.get('centaeris.acceptance.project') != self.manifest['project']:
            raise RuntimeError('Runtime does not belong to exact fixture')
        return container, state

    def restart(self):
        container, before = self.runtime_state()
        expected = {row['containerId'] for row in self.last.values()}
        self.dc('restart', 'runtime')
        _, after = self.wait(self.runtime_state, lambda item: item[1]['Running'] and item[1]['StartedAt'] != before['StartedAt'],
                             'new Runtime process')
        for label, row in self.last.items():
            self.paused(label, row)
        return {'performed': True, 'beforeRuntimeStartedAt': before['StartedAt'],
                'afterRuntimeStartedAt': after['StartedAt'], 'preservedContainers': sorted(expected)}

    def cancellation(self):
        holder = self.admit('A', 'RUNSC_ROTATION_BLOCK ' + self.tag + ' A')
        self.wait(lambda: self.status(holder), lambda row: row['runStatus'] == 'running' and row['highestRunSequence'] > 0,
                  'holder Run admission')
        self.wait(lambda: self.observe('A', holder), lambda row: not row['paused'] and row['runningReservations'] == 1,
                  'holder running-slot reservation')
        queued = self.admit('B', f'RUNSC_ROTATION {self.tag} B 11')
        self.http(f"/api/sessions/{queued['sessionId']}/agent-runs/{queued['agentRunId']}/cancel", {})
        cancelled = self.wait(lambda: self.status(queued), lambda row: row['runStatus'] == 'cancelled', 'queued cancellation')
        queued_observation = self.observe('B', queued)
        if 'oracle' in queued_observation or queued_observation['successfulCommands'] != 0:
            raise RuntimeError('cancelled queued Run executed the oracle')
        self.http(f"/api/sessions/{holder['sessionId']}/agent-runs/{holder['agentRunId']}/cancel", {})
        held = self.wait(lambda: self.status(holder), lambda row: row['runStatus'] == 'cancelled', 'holder cancellation')
        after = self.paused('A', holder)
        return {'queuedRunStatus': cancelled['runStatus'], 'queuedSuccessfulCommands': queued_observation['successfulCommands'],
                'holderRunStatus': held['runStatus'], 'runningReservationsAfter': after['runningReservations']}

    def retire(self, row):
        body = {'schema': 'runtime.environment.retire.v1', 'operationId': 'rotation-retire-' + uuid.uuid4().hex,
                'environmentId': row['environmentId'], 'instanceId': row['instanceId'], 'hostId': row['hostId'],
                'expectedControllerEpoch': row['controllerEpoch'], 'acknowledgeTmpfsLoss': True}
        request = urllib.request.Request(f"http://127.0.0.1:{self.manifest['runtimePort']}/internal/environments/retire",
            data=json.dumps(body).encode(), headers={'Content-Type': 'application/json',
                'x-internal-token': self.private['INTERNAL_API_TOKEN'],
                'x-environment-maintenance-token': self.private['ENVIRONMENT_MAINTENANCE_TOKEN']})
        def invoke():
            with urllib.request.urlopen(request, timeout=60) as response:
                return json.load(response)
        result = self.wait(invoke, lambda value: value.get('status') == 'destroyed', 'exact environment retirement')
        if any(result.get(key) != body[key] for key in ('operationId', 'environmentId', 'instanceId')):
            raise RuntimeError('retirement response identity mismatch')
        inventory = self.command(self.docker + ['ps', '-aq', '--no-trunc']).splitlines()
        code = r'''import json
from django.db import connection,transaction
with transaction.atomic():
 with connection.cursor() as c:
  c.execute('SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY')
  c.execute('SELECT count(*) FROM runtime.resident_sandboxes WHERE environment_id=%s',[ENV])
  count=c.fetchone()[0]
  c.execute('SELECT count(*) FROM runtime.execution_instances WHERE environment_id=%s AND instance_id=%s',[ENV,INSTANCE])
  history=c.fetchone()[0]>0
  print(json.dumps({'residentHoldingCount':count,'historyPreserved':history}))
'''.replace('ENV', repr(row['environmentId'])).replace('INSTANCE', repr(row['instanceId']))
        proof = self.sql(code)
        return {'environmentId': row['environmentId'], 'status': result['status'],
                'containerAbsent': row['containerId'] not in inventory, **proof}

    def run(self):
        evidence = {'schema': SCHEMA, 'sourceRevision': self.manifest['sourceRevision'], 'runningLimit': 1, 'runs': []}
        try:
            index = 0
            while index < 12:
                pending = []
                label = 'A' if index % 2 == 0 else 'B'
                admitted = self.admit(label, f'RUNSC_ROTATION {self.tag} {label} {index}')
                pending.append((index, label, admitted))
                if index == 4:
                    self.wait(lambda: self.observe('A', admitted),
                              lambda row: not row['paused'] and row['runningReservations'] == 1,
                              'busy environment holding the only running slot')
                    queued = self.admit('B', f'RUNSC_ROTATION {self.tag} B 5')
                    waiting = self.wait(lambda: self.observe('B', queued),
                                        lambda row: row['paused'] and row['powerState'] == 'paused'
                                        and row['runningReservations'] == 1 and 'oracle' not in row,
                                        'queued original demand behind the busy environment', timeout=10)
                    pending.append((5, 'B', queued))
                    evidence['wakeup'] = {'waitingRunId': queued['agentRunId'], 'waitObserved': True,
                                          'waitingPaused': waiting['paused'],
                                          'runningReservations': waiting['runningReservations'],
                                          'completedWithoutReadmission': False}
                statuses = [(number, name, run, self.completed(run)) for number, name, run in pending]
                for number, name, run, status in statuses:
                    row = {**self.paused(name, run), **status}
                    evidence['runs'].append(row)
                    self.last[name] = row
                    (self.result / f'run-{number:02d}.json').write_text(json.dumps(row, indent=2))
                if index == 3:
                    evidence['cancellation'] = self.cancellation()
                if index == 4:
                    evidence['wakeup']['completedWithoutReadmission'] = True
                    evidence['restart'] = self.restart()
                index += len(pending)
            evidence['retirements'] = [self.retire(row) for row in self.last.values()]
            summary = validate_evidence(evidence)
            (self.result / 'summary.json').write_text(json.dumps(summary, indent=2))
            return summary
        finally:
            (self.result / 'observations.json').write_text(json.dumps(evidence, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fixture', type=Path)
    parser.add_argument('--evidence', type=Path, help='verify existing observations without any network or Docker call')
    parser.add_argument('--result', type=Path, required=True, help='new private result directory, outside source checkout')
    parser.add_argument('--acknowledge-fixture-retirement', action='store_true')
    args = parser.parse_args()
    if bool(args.fixture) == bool(args.evidence):
        parser.error('select exactly one of --fixture or --evidence')
    if args.evidence:
        summary = validate_evidence(json.loads(args.evidence.read_text()))
        args.result.mkdir(mode=0o700, parents=False, exist_ok=False)
        (args.result / 'summary.json').write_text(json.dumps(summary, indent=2))
    else:
        if not args.acknowledge_fixture_retirement:
            parser.error('--fixture requires --acknowledge-fixture-retirement for its two exact environments')
        summary = FixtureDriver(args.fixture, args.result).run()
    print(json.dumps(summary))


if __name__ == '__main__':
    main()
