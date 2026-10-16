"""Opt-in fixture boundary: no production hook, no implicit task replay."""
import json
import re
import time


def parse_marker(marker):
    match=re.fullmatch(r'ACCEPTANCE_BOUNDARY_RUN steps=(\d+) index=(\d+) nonce=([a-f0-9]{32}) timeout=(\d+)',marker)
    if match is None:raise ValueError('invalid explicit boundary goal')
    steps,index,nonce,timeout=match.groups();steps,index,timeout=int(steps),int(index),int(timeout)
    if not 8<=steps<=32 or not 1<=index<steps-1 or not 60<=timeout<=300:
        raise ValueError('boundary steps/index/timeout outside supported fixture bounds')
    return {'steps':steps,'index':index,'nonce':nonce,'timeout':timeout}


def marker_for(steps,nonce,timeout):
    marker=f'ACCEPTANCE_BOUNDARY_RUN steps={steps} index={steps//2} nonce={nonce} timeout={timeout}'
    parse_marker(marker)
    return marker


def call_id(config,index):
    return 'call-boundary-'+config['nonce']+'-%04d'%index


def token(config,index):return 'acceptance-boundary-'+config['nonce']+'-%04d'%index


def paths(config):
    root='/mnt/data/retained-boundary-'+config['nonce']
    return root+'-markers.txt',root+'-release'


def release_token(config):return call_id(config,config['index'])+':'+config['nonce']+':%04d'%config['index']


def command_for(config,index):
    if type(index) is not int or not 0<=index<config['steps']:raise ValueError('invalid boundary command index')
    markers,release=paths(config)
    command="printf '%%s\\n' %04d >> %s; "%(index,markers)
    if index==config['index']:
        command+="printf '%%s\\n' %s; n=0; while ! test -f %s; do n=$((n+1)); test \"$n\" -lt %d || exit 73; sleep 1; done; test \"$(cat %s)\" = %s || exit 74; "%(release_token(config),release,config['timeout'],release,release_token(config))
    return command+'printf '+token(config,index)


def next_boundary_call(request,marker):
    config=parse_marker(marker)
    messages=request['messages']
    if not any('Keep all acceptance commands bounded.' in str(message.get('content','')) for message in messages):
        raise ValueError('boundary model request lacks actual AGENTS.md instructions')
    last_user=max(i for i,m in enumerate(messages) if m.get('role')=='user')
    replies=[m for m in messages[last_user+1:] if m.get('role')=='tool']
    index=0
    if replies:
        matches=re.findall(r'(?<![A-Za-z0-9_-])acceptance-boundary-'+config['nonce']+r'-(\d{4})(?![A-Za-z0-9_-])',str(replies[-1].get('content','')))
        if len(matches)!=1:raise ValueError('boundary lacks its unique actual command output')
        index=int(matches[0])+1
    if index==config['steps']:return None
    if not 0<=index<config['steps']:raise ValueError('boundary progress exceeded its goal')
    return 'bash',{'command':command_for(config,index),'timeout_ms':(config['timeout']+30)*1000}


def validate_facts(state,config,run,complete=False):
    if state['agentRunId']!=run['agentRunId'] or state['sessionId']!=run['sessionId']:
        raise RuntimeError('boundary canonical Run/session mismatch')
    if state['runStatus'] not in ('running','completed'):raise RuntimeError('boundary Run failed or stopped')
    calls=state['calls'];results=state['results'];target=config['steps'] if complete else config['index']+1
    if len(calls)<target:return None
    if len(calls)!=target:raise RuntimeError('boundary advanced beyond its blocked call')
    ids=set();previous=0
    for index,call in enumerate(calls):
        expected_id=call_id(config,index)
        if (call['callId']!=expected_id or expected_id in ids or not call['turnId']
                or call['sequence']<=previous or call['toolName']!='bash'
                or call['normalizedInput'].get('command')!=command_for(config,index)):
            raise RuntimeError('boundary canonical call identity/command/order mismatch')
        ids.add(expected_id);previous=call['sequence']
        paired=[r for r in results if r['callId']==expected_id]
        if not complete and index==config['index']:
            if paired:raise RuntimeError('boundary result committed before release')
        elif (len(paired)!=1 or paired[0]['turnId']!=call['turnId']
                or paired[0]['sequence']<=call['sequence']
                or (index+1<len(calls) and paired[0]['sequence']>=calls[index+1]['sequence'])
                or paired[0]['resultState']!='successWithOutput'
                or token(config,index) not in paired[0]['modelContent']):
            raise RuntimeError('boundary lacks exact successful original call/result pairing')
    if any(r['callId'] not in ids for r in results):raise RuntimeError('unknown boundary result identity')
    expected_results=target if complete else target-1
    if len(results)!=expected_results:raise RuntimeError('duplicate/missing boundary canonical results')
    return calls[-1] if complete else calls[config['index']]


class BoundaryNotReady(Exception):
    """Only an admitted running task whose first output is not flushed yet."""


def running_task(response,config,run_id,expected=None,allow_unflushed=False):
    if response.get('status')!='success' or response.get('result',{}).get('kind')!='task':
        raise RuntimeError('boundary task query failed')
    result=response['result'];task=result['task']
    if (task['scope']!={'kind':'run','runId':run_id} or task['state']!='running'
            or task['cleanupComplete'] is not False or not task.get('operationId')
            or (expected is not None and any(task[k]!=expected[k] for k in ('taskId','operationId','scope')))
            or result.get('gap') is not False or result.get('earliestCursor')!=0):
        raise RuntimeError('original boundary task is missing/replaced/not running')
    try:output=b''.join(bytes(row['data']) for row in result['output'] if row['stream']=='stdout').decode('utf-8')
    except (ValueError,TypeError,UnicodeError) as error:raise RuntimeError('invalid boundary task output') from error
    if output=='' and allow_unflushed:raise BoundaryNotReady('boundary start output not flushed yet')
    if output!=release_token(config)+'\n':raise RuntimeError('boundary task did not expose its exact start nonce/call/index')
    return task


def read_state(compose,run,config,command):
    # Only this owned fixture and its one Run are read. No snapshot hydration or
    # history body is loaded: at most32 calls, results and scalar model counts.
    code="""
import json
from django.conf import settings
from django.db import connection,transaction
from app_core.models import AgentRun,SessionEvent
assert settings.DATABASES['default']['NAME']=='retained_acceptance'
with transaction.atomic():
 with connection.cursor() as c:c.execute('SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY')
 run=AgentRun.objects.select_related('environment').get(id=RUN_LITERAL)
 env=run.environment
 if run.status=='queued' or env.controller_run_id!=run.id:
  print(json.dumps({'notReady':True}));raise SystemExit(0)
 with connection.cursor() as c:
  c.execute("SELECT container_id,instance_id FROM runtime.resident_sandboxes WHERE environment_id=%s AND state='resident'",[env.id]);residents=c.fetchall()
 assert len(residents)==1
 calls=[];results=[]
 rows=SessionEvent.objects.filter(agent_run=run,session_level=False,payload__type__in=['tool_call','tool_result']).order_by('agent_run_sequence')
 ids=set()
 for row in rows:
  wire=row.payload;value=wire['payload']
  if wire['type']=='tool_call' and value.get('toolName')=='bash' and NONCE_LITERAL in value.get('normalizedInput',{}).get('command',''):
   assert len(calls)<32
   calls.append({**value,'turnId':wire.get('turnId'),'sequence':row.agent_run_sequence});ids.add(value['callId'])
  elif wire['type']=='tool_result' and value.get('callId','').startswith('call-boundary-'+NONCE_LITERAL+'-'):
   assert len(results)<32
   results.append({**value,'turnId':wire.get('turnId'),'sequence':row.agent_run_sequence})
 main_count=SessionEvent.objects.filter(agent_run=run,payload__type='model_request_started',payload__payload__purpose='main').count()
 print(json.dumps({'agentRunId':run.id,'sessionId':run.session_id,'environmentId':env.id,'containerId':residents[0][0],'instanceId':residents[0][1],'runStatus':run.status,'mainRequestCount':main_count,'calls':calls,'results':results,'fence':{'environmentInstanceId':residents[0][1],'controllerId':run.id,'controllerEpoch':env.controller_epoch,'controllerPurpose':env.controller_purpose}}))
""".replace('RUN_LITERAL',repr(run['agentRunId'])).replace('NONCE_LITERAL',repr(config['nonce']))
    result=command(['docker','compose','-f',str(compose),'exec','-T','api','/app/.venv/bin/python','manage.py','shell','-c',code])
    if result.returncode:raise RuntimeError('boundary canonical observation failed')
    state=json.loads(next(line for line in reversed(result.stdout.splitlines()) if line.startswith('{')))
    return None if state.get('notReady') else state


def execute_restart(run,config,previous,observe,runtime_state,restart,rpc,release,save,probe):
    deadline=time.monotonic()+config['timeout']
    before=None;original=None;call=None
    while time.monotonic()<deadline:
        state=observe()
        if state is not None:
            call=validate_facts(state,config,run)
            if call is not None:
                for key in ('containerId','instanceId','environmentId','sessionId'):
                    if state[key]!=previous[key]:raise RuntimeError('boundary physical continuity mismatch')
                fence=state['fence']
                if fence['controllerPurpose']!='execute':raise RuntimeError('boundary lost execute authority')
                listed=rpc(state['containerId'],fence,'list')
                if listed.get('status')!='success':raise RuntimeError('boundary current task list failed')
                tasks=[t for t in listed['result']['tasks'] if t['scope']=={'kind':'run','runId':run['agentRunId']} and t['state']=='running']
                if len(tasks)>1:raise RuntimeError('ambiguous running boundary tasks')
                if tasks:
                    try:
                        task=running_task(rpc(state['containerId'],fence,'query',taskId=tasks[0]['taskId'],cursor=0,maxBytes=4096),config,run['agentRunId'],allow_unflushed=True)
                    except BoundaryNotReady:
                        time.sleep(.25);continue
                    before=state;original=task;break
        time.sleep(.25)
    if before is None:raise RuntimeError('exact running-tool boundary not reached within timeout; fixture preserved')
    old_runtime=runtime_state()
    if not old_runtime.get('Running') or old_runtime.get('OOMKilled'):raise RuntimeError('boundary Runtime was not healthy')
    save('boundary-before-restart.json',{'state':before,'task':original,'call':call,'runtime':old_runtime,'config':config})
    restart()
    new_runtime=runtime_state()
    if (not new_runtime.get('Running') or new_runtime.get('OOMKilled')
            or new_runtime['Pid']==old_runtime['Pid'] or new_runtime['StartedAt']==old_runtime['StartedAt']):
        raise RuntimeError('boundary Runtime did not actually restart')
    save('boundary-runtime-restarted.json',{'before':old_runtime,'after':new_runtime})
    while time.monotonic()<deadline:
        state=observe()
        if state is not None:
            if any(state[k]!=before[k] for k in ('agentRunId','sessionId','environmentId','instanceId','containerId')):
                raise RuntimeError('boundary takeover changed original identity')
            current=state['fence'];old=before['fence']
            if current['controllerPurpose']=='execute' and current['controllerEpoch']>old['controllerEpoch']:
                if validate_facts(state,config,run)!=call or state['mainRequestCount']!=before['mainRequestCount']:
                    raise RuntimeError('boundary advanced/recalled model before original result')
                running_task(rpc(state['containerId'],current,'query',taskId=original['taskId'],cursor=0,maxBytes=4096),config,run['agentRunId'],original)
                stale=rpc(state['containerId'],old,'query',taskId=original['taskId'],cursor=0,maxBytes=4096)
                if stale.get('status')!='error' or stale.get('error')!='task_controller_fenced':raise RuntimeError('old boundary query fence not rejected')
                rejection=probe(state['containerId'],old,current,previous['backgroundTaskIds'][0])
                if deadline-time.monotonic()<5:raise RuntimeError('insufficient boundary wait budget; no release')
                running_task(rpc(state['containerId'],current,'query',taskId=original['taskId'],cursor=0,maxBytes=4096),config,run['agentRunId'],original)
                proof={'config':config,'agentRunId':run['agentRunId'],'callId':call['callId'],'turnId':call['turnId'],'callSequence':call['sequence'],'task':original,'beforeFence':old,'afterFence':current,'runtimeBefore':old_runtime,'runtimeAfter':new_runtime,'oldQuery':stale,'staleControlProof':rejection,'released':False}
                save('boundary-before-release.json',proof)
                release(state['containerId'],config)
                proof['released']=True
                save('boundary-restart-proof.json',proof)
                return proof
        time.sleep(.25)
    raise RuntimeError('same-task takeover not proven before bounded barrier timeout; no release')
