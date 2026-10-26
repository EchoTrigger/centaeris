"""Observe durable completion and continuity of K6-created Runs; no load generation."""
from collections import Counter
import importlib.util
import json
import os
import sys
from pathlib import Path
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

def validate_run_facts(rows, environments, iterations):
    if (type(environments) is not int or not 1 <= environments <= 64
            or type(iterations) is not int or not 1 <= iterations <= 20
            or not isinstance(rows, list) or len(rows) != environments * iterations):
        raise ValueError('exact planned Run cardinality required')
    runs, sessions, instances = set(), {}, {}
    for row in rows:
        if (not isinstance(row, dict) or any(not isinstance(row.get(key), str) or not row[key]
                for key in ('agentRunId', 'sessionId', 'environmentId', 'instanceId'))
                or row['agentRunId'] in runs or row.get('runStatus') != 'completed'
                or row.get('coreTerminalType') != 'agent_run_completed'
                or row.get('cleanupComplete') is not True
                or type(row.get('releaseReceiptCount')) is not int or row['releaseReceiptCount'] != 1):
            raise ValueError('exact successful Core terminal and cleanup facts required')
        runs.add(row['agentRunId'])
        binding = (row['environmentId'], row['instanceId'])
        if sessions.setdefault(row['sessionId'], binding) != binding:
            raise ValueError('Session original environment and instance changed')
        if instances.setdefault(row['instanceId'], row['environmentId']) != row['environmentId']:
            raise ValueError('instance shared between different environments')
    counts = Counter(row['sessionId'] for row in rows)
    if (len(sessions) != environments or len({value[0] for value in sessions.values()}) != environments
            or len(instances) != environments or any(count != iterations for count in counts.values())):
        raise ValueError('independent Sessions with the exact rolling Run count required')
    return {'completedRuns': len(rows), 'sameInstanceEnvironments': environments, 'cleanupCompleteRuns': len(rows)}


def run(fixture, result, k6binary, *, environments=4, iterations=3, work_units=2000,
        between_run_ms=6000, model_wait_ms=0, initial_stagger_ms=0,
        expected_working_environments=None, run_terminal_timeout_seconds=120, dependencies=None):
    if type(run_terminal_timeout_seconds) is not int or not 1 <= run_terminal_timeout_seconds <= 600:
        raise ValueError('bounded per-Run terminal timeout required')
    if (type(environments) is not int or not 2 <= environments <= 64
            or type(iterations) is not int or not 1 <= iterations <= 20
            or type(work_units) is not int or not 1 <= work_units <= 1000000
            or type(between_run_ms) is not int or not 0 <= between_run_ms <= 60000):
        raise ValueError('bounded explicit shared workload required')
    expected_working = environments if expected_working_environments is None else expected_working_environments
    if (type(model_wait_ms) is not int or not 0<=model_wait_ms<=120000
            or type(initial_stagger_ms) is not int or not 0<=initial_stagger_ms<=120000
            or initial_stagger_ms*(environments-1)>120000
            or type(expected_working) is not int or not 1<=expected_working<=environments):
        raise ValueError('bounded shared wait, initial stagger and overlap threshold required')
    dependencies = dependencies or _dependencies()
    fixture, result = Path(fixture), Path(result)
    driver = dependencies['driver'](fixture, result, expected_running_limit=environments)
    if (driver.manifest.get('sharedWorkload', {}).get('environmentCount') != environments
            or driver.manifest.get('sharedWorkload', {}).get('modelWaitMs',0) != model_wait_ms):
        raise RuntimeError('shared fixture environment count mismatch')
    report = {'schema': 'runsc.shared.workload.acceptance.v1', 'status': 'failedPreserved',
              'sourceRevision': driver.manifest['sourceRevision'], 'environments': environments,
              'iterations': iterations, 'runTerminalTimeoutSeconds':run_terminal_timeout_seconds, 'workUnits': work_units, 'betweenRunMs': between_run_ms,
              'modelWaitMs':model_wait_ms,'initialStaggerMs':initial_stagger_ms,
              'initialStaggerTotalMs':initial_stagger_ms*(environments-1),'expectedWorkingEnvironments':expected_working,
              'memoryContinuity': 'notObserved', 'clientFirstToolLatency': 'notObserved',
              'resumeToFirstToolLatency': 'notObserved', 'durableTiming': 'notObserved',
              'retirement': [], 'responsibility': 'preserved'}
    samples = [{'sampleId': uuid.uuid4().hex, 'operationId': uuid.uuid4().hex, 'workUnits': work_units}
               for _ in range(environments * iterations)]
    private = {'schema': 'workspace.perf.fixture.v1', 'nativeManifest': str((fixture / 'manifest.json').resolve()),
               'apiBase': driver.base, 'workspaceId': driver.seed['workspaceId'], 'agentId': driver.seed['agentId'],
               'modelRefs': {'tools': driver.seed['modelConfigRef']},
               'account': {key: driver.seed[key] for key in ('email', 'password')}, 'samples': samples}
    private_path = fixture / 'perf-fixture.json'
    descriptor = os.open(private_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, 'w') as stream:
        json.dump(private, stream)
    observer = dependencies['observer'](fixture / 'manifest.json', result / 'native-observations.jsonl', environments, expected_working_environments=expected_working)
    started, stopped = False, False
    began = time.monotonic()
    try:
        observer.start(); started = True
        client = dependencies['workload'](private_path, result / 'k6', k6binary,
                    environments=environments, iterations=iterations, timeout=900, between_run_ms=between_run_ms, initial_stagger_ms=initial_stagger_ms, run_terminal_timeout_seconds=run_terminal_timeout_seconds)
        report['client'] = client
        report['clientFirstToolLatency'] = client.get('firstSuccessfulToolResultLatency', 'notObserved')
        if client.get('status') != 'clientVerified':
            raise RuntimeError('K6 did not verify the full workload')
        records = [json.loads(line) for line in (result / 'k6/run-records.jsonl').read_text().splitlines() if line]
        admitted = [record for record in records if record.get('phase') == 'accepted']
        rows = [driver.completed(record) for record in admitted]
        report['runs'] = rows
        report.update(validate_run_facts(rows, environments, iterations))
        if hasattr(driver, 'sql'):
            report['durableTiming'] = _durable_timing(driver, admitted)
        last = {record['sessionId']: record for record in admitted}
        final = [driver.wait(lambda label=str(index), run=record: driver.observe(label, run),
                            lambda row: row.get('paused') is True and row.get('powerState') == 'paused',
                            'final exact environment automatic pause')
                 for index, record in enumerate(last.values())]
        report['native'] = observer.stop(); stopped = True
        if (report['native'].get('physicalOverlap') != 'verified'
                or report['native'].get('peakWorkingEnvironments', 0) < expected_working):
            raise RuntimeError('explicit working-environment overlap threshold was not observed')
        if iterations > 1 and (report['native'].get('pausedEnvironmentInstances', 0) < environments
                or report['native'].get('resumedEnvironmentInstances', 0) < environments):
            raise RuntimeError('every original environment must be physically observed paused and resumed')
        expected = {row['environmentId']: row['instanceId'] for row in rows}
        for row in final:
            if (expected.get(row.get('environmentId')) != row.get('instanceId')
                    or row.get('runtime') != 'runsc' or row.get('running') is not True):
                raise RuntimeError('final physical environment identity unavailable')
        report['finalEnvironments'] = final
        for row in final:
            retired = driver.retire(row)
            report['retirement'].append(retired)
            if (retired.get('status') != 'destroyed' or retired.get('containerAbsent') is not True
                    or retired.get('residentHoldingCount') != 0 or retired.get('historyPreserved') is not True):
                raise RuntimeError('positive product retirement and history evidence required')
        report.update(status='passed', responsibility='released')
        return report
    except BaseException as error:
        report['errorType'] = type(error).__name__
        raise
    finally:
        if started and not stopped:
            try: report['native'] = observer.stop()
            except BaseException as error: report['observerErrorType'] = type(error).__name__
        report['wallTimeMs'] = int((time.monotonic() - began) * 1000)
        (result / 'shared-report.json').write_text(json.dumps(report, indent=2))


def _dependencies():
    source = Path(__file__).resolve().parents[3]
    def load(name, path):
        spec = importlib.util.spec_from_file_location(name, path)
        module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
        return module
    rotation = load('shared_rotation_driver', source / 'scripts/workspace/runsc_rotation_acceptance.py')
    workload = load('shared_k6_workload', source / 'perf/harness/fixture_workload.py')
    observer = load('shared_native_observer', source / 'perf/harness/native_observer.py')
    return {'driver': rotation.FixtureDriver, 'workload': workload.run, 'observer': observer.NativeObserver}


def _durable_timing(driver, admitted):
    # Only timestamp projections are selected; model content, credentials and
    # authorization records never enter the timing report.
    code = '''import json
from django.conf import settings
from django.db import connection,transaction
from app_core.models import AgentRun,AgentRunEnvironmentRelease,SessionEvent
assert settings.DATABASES['default']['NAME']=='retained_acceptance'
def millis(value): return int(value.timestamp()*1000) if value else None
with transaction.atomic():
 with connection.cursor() as cursor:
  cursor.execute('SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY')
  rows=[]
  for run in AgentRun.objects.filter(pk__in=RUN_IDS).order_by('createdAt'):
   first=SessionEvent.objects.filter(agent_run=run,session_level=False,payload__type='tool_call',payload__payload__toolName='bash').order_by('agent_run_sequence').values_list('createdAtMs',flat=True).first()
   execution_fact=SessionEvent.objects.filter(agent_run=run,session_level=False,payload__type='agent_run_execution_started').order_by('agent_run_sequence').values_list('createdAtMs',flat=True).first()
   execution_fact=execution_fact if type(execution_fact) is int and execution_fact>=0 else None
   calls={}
   for call_id,tool_name,sequence,created in SessionEvent.objects.filter(agent_run=run,session_level=False,payload__type='tool_call').order_by('agent_run_sequence').values_list('payload__payload__callId','payload__payload__toolName','agent_run_sequence','createdAtMs'):
    if isinstance(call_id,str) and call_id and isinstance(tool_name,str) and tool_name and type(sequence) is int and type(created) is int and created>=0:
     calls[call_id]=(tool_name,sequence,created) if call_id not in calls else None
   first_successful_result=None
   for call_id,tool_name,sequence,created in SessionEvent.objects.filter(agent_run=run,session_level=False,payload__type='tool_result',payload__payload__resultState__in=['successWithOutput','successNoOutput']).order_by('agent_run_sequence').values_list('payload__payload__callId','payload__payload__toolName','agent_run_sequence','createdAtMs'):
    call=calls.get(call_id) if isinstance(call_id,str) else None
    if call and call[0]==tool_name and type(sequence) is int and call[1]<sequence and type(created) is int and created>=call[2]:
     first_successful_result=created;break
   execution_to_result=(first_successful_result-execution_fact if execution_fact is not None and first_successful_result is not None and first_successful_result>=execution_fact else None)
   release=AgentRunEnvironmentRelease.objects.filter(agent_run=run).values_list('recorded_at',flat=True).first()
   cursor.execute('SELECT recorded_at_ms FROM runtime.agent_run_cleanup_receipts WHERE agent_run_id=%s AND cleanup_complete',[run.id]);cleanup=cursor.fetchone()
   rows.append({'agentRunId':run.id,'createdAtMs':millis(run.createdAt),'startedAtMs':millis(run.startedAt),'completedAtMs':millis(run.completedAt),'durableFirstBashCallAtMs':first,'durableExecutionFactCreatedAtMs':execution_fact,'durableFirstSuccessfulToolResultCreatedAtMs':first_successful_result,'executionFactToFirstToolResultMs':execution_to_result,'businessReleaseAtMs':millis(release),'cleanupReceiptAtMs':cleanup[0] if cleanup else None,'runWallTimeMs':millis(run.completedAt)-millis(run.startedAt) if run.startedAt and run.completedAt else None})
  jobs={('agent_run.lifecycle:'+run_id):run_id for run_id in RUN_IDS}
  cursor.execute('SELECT task_id,payload_json FROM runtime.runtime_events WHERE task_id=ANY(%s) AND event_type=%s ORDER BY at_ms,event_id',[list(jobs),'runtime.resident_capacity.claimed.v1'])
  validated_claims=[]
  for job_id,payload in cursor.fetchall():
   event=json.loads(payload) if isinstance(payload,str) else payload
   released,claimed,generation=event.get('releaseRecordedAtMs'),event.get('claimedAtMs'),event.get('generation')
   if (event.get('schema')!='runtime.resident_capacity.claimed.v1' or event.get('jobId')!=job_id or job_id not in jobs or type(generation) is not int or generation<0 or type(released) is not int or type(claimed) is not int or not 0<=released<=claimed):
    raise RuntimeError('capacity claim timing identity invalid')
   validated_claims.append((job_id,event))
  generations={event['generation'] for _,event in validated_claims}
  preparations={}
  if generations:
   cursor.execute("SELECT event_id,task_id,at_ms,payload_json FROM runtime.runtime_events WHERE event_type=%s AND (event_id=ANY(%s) OR (payload_json::jsonb->>'generation')=ANY(%s))",['runtime.host_preparation.available.v1',['host_preparation_available:'+str(generation) for generation in sorted(generations)],[str(generation) for generation in sorted(generations)]])
   for event_id,host_id,at_ms,payload in cursor.fetchall():
    available=json.loads(payload) if isinstance(payload,str) else payload
    if not isinstance(available,dict) or set(available)!={'schema','scope','hostId','generation','availableAtMs'}:
     raise RuntimeError('host preparation availability identity invalid')
    generation=available['generation']
    if (available['schema']!='runtime.host_preparation.available.v1' or available['scope']!='globalPreparation' or type(generation) is not int or generation not in generations or generation in preparations or not isinstance(available['hostId'],str) or not available['hostId'].strip() or host_id!=available['hostId'] or event_id!='host_preparation_available:'+str(generation) or type(at_ms) is not int or type(available['availableAtMs']) is not int or at_ms!=available['availableAtMs'] or at_ms<0):
     raise RuntimeError('host preparation availability identity invalid')
    preparations[generation]=available
  claims=[]
  for job_id,event in validated_claims:
   released,claimed,generation=event['releaseRecordedAtMs'],event['claimedAtMs'],event['generation']
   row={'agentRunId':jobs[job_id],'jobId':job_id,'generation':generation,'claimedAtMs':claimed}
   available=preparations.get(generation)
   if available is not None:
    if available['availableAtMs']!=released or available['availableAtMs']>claimed:
     raise RuntimeError('host preparation availability timestamp mismatch')
    row.update(wakeSource='hostPreparation',hostId=available['hostId'],availabilityScope=available['scope'],hostPreparationAvailableAtMs=available['availableAtMs'],hostPreparationToClaimMs=claimed-available['availableAtMs'])
   else:
    row.update(wakeSource='physicalRelease',releaseRecordedAtMs=released,releaseToClaimMs=claimed-released)
   claims.append(row)
  physical=[event['releaseToClaimMs'] for event in claims if event['wakeSource']=='physicalRelease']
  preparation=[event['hostPreparationToClaimMs'] for event in claims if event['wakeSource']=='hostPreparation']
  print(json.dumps({'schema':'runsc.shared.durable_timing.v1','toolTimingBasis':'committedSessionEvent.createdAtMs','rows':rows,'capacityClaims':claims,'releaseToClaimMs':physical if physical else 'notObserved','hostPreparationToClaimMs':preparation if preparation else 'notObserved','clientFirstToolLatency':'notObserved','resumeToFirstToolLatency':'notObserved'}))
'''.replace('RUN_IDS', repr([record['agentRunId'] for record in admitted]))
    return driver.sql(code)


def main():
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fixture', required=True, type=Path)
    parser.add_argument('--result', required=True, type=Path)
    parser.add_argument('--k6-binary', required=True, type=Path)
    parser.add_argument('--environment-count', type=int, default=4)
    parser.add_argument('--iterations', type=int, default=3)
    parser.add_argument('--work-units', type=int, default=2000)
    parser.add_argument('--between-run-ms', type=int, default=6000)
    parser.add_argument('--model-wait-ms', type=int, default=0)
    parser.add_argument('--initial-stagger-ms', type=int, default=0)
    parser.add_argument('--run-terminal-timeout-seconds', type=int, default=120)
    parser.add_argument('--expected-working-environments', type=int)
    args = parser.parse_args()
    report = run(args.fixture, args.result, args.k6_binary, environments=args.environment_count,
                 iterations=args.iterations, work_units=args.work_units, between_run_ms=args.between_run_ms,
                 model_wait_ms=args.model_wait_ms,initial_stagger_ms=args.initial_stagger_ms,
                 expected_working_environments=args.expected_working_environments,run_terminal_timeout_seconds=args.run_terminal_timeout_seconds)
    print(json.dumps(report))


if __name__ == '__main__': main()
