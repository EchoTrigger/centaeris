"""Observe durable completion and continuity of K6-created Runs; no load generation."""
from collections import Counter
import importlib.util
import json
import os
import signal
import subprocess
import threading
import sys
from pathlib import Path
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from runsc_acceptance.workload_budget import FINAL_JOB_SETTLE_SECONDS, workload_budget
from runsc_acceptance.memory_profile import memory_profile

_PREPARATION_PHASES = frozenset({'preparationAttempt', 'environmentAcquire', 'globalAcquire', 'globalHold',
    'preparationInventory', 'preparationReconcileReserve', 'physicalConfirm', 'controllerTakeover', 'workspacePreparation'})
_PREPARATION_LOG_TIMEOUT_SECONDS = 5
_PREPARATION_LOG_MAX_BYTES = 32 * 1024 * 1024
_PREPARATION_LOG_MAX_LINE_BYTES = 64 * 1024
_PREPARATION_LOG_MAX_RECORDS = 20000
_YIELD_REASONS = frozenset({'environment_controller_wait', 'execution_host_preparation_wait', 'execution_host_admission_wait',
    'execution_resident_capacity_wait', 'runsc_environment_capacity_wait',
    'execution_recovery_checkpoint_committed', 'question_wait', 'runtime_job_wait',
    'session_record_commit_unavailable', 'session_workspace_commit_unavailable',
    'session_workspace_resolve_unavailable', 'control_retry_pending', 'control_cleanup_pending'})


def _preparation_record(line, jobs):
    if len(line) > _PREPARATION_LOG_MAX_LINE_BYTES:
        raise ValueError('preparation observation line exceeded bound')
    try:
        event = json.loads(line)
    except (json.JSONDecodeError, UnicodeDecodeError):
        return None
    if (not isinstance(event, dict) or event.get('schema') != 'workspace.perf.v1' or event.get('source') != 'runtime'
            or not isinstance(event.get('jobId'), str) or event['jobId'] not in jobs
            or not isinstance(event.get('phase'), str) or event['phase'] not in _PREPARATION_PHASES
            or not isinstance(event.get('outcome'), str) or event['outcome'] not in {'ok', 'error', 'interrupted'}
            or type(event.get('elapsedMs')) not in (int, float) or not 0 <= event['elapsedMs'] <= 14400000
            or type(event.get('atUnixMs')) is not int or not 0 <= event['atUnixMs'] < 2**64):
        return None
    return {key: event[key] for key in ('jobId', 'phase', 'elapsedMs', 'outcome', 'atUnixMs')}


def _preparation_timing(driver, record_path):
    """Retain only allowlisted timing values; raw process output never leaves this reader."""
    try:
        accepted = [json.loads(line) for line in record_path.read_text(encoding='utf-8').splitlines() if line]
        jobs = {'agent_run.lifecycle:' + record['agentRunId'] for record in accepted
            if isinstance(record, dict) and record.get('phase') == 'accepted'
            and isinstance(record.get('agentRunId'), str) and record['agentRunId']}
        if not jobs:
            return 'notObserved'
        options = {'stdin': subprocess.DEVNULL, 'stdout': subprocess.PIPE, 'stderr': subprocess.DEVNULL,
            'env': driver.environment}
        if os.name == 'posix':
            # Only this log-reader command and its CLI plugin children belong to the group.
            options['start_new_session'] = True
        process = subprocess.Popen([*driver.docker, 'compose', '-f', str(driver.compose),
            'logs', '--no-color', '--no-log-prefix', 'runtime'], **options)
        expired, stopped = threading.Event(), threading.Event()
        def stop():
            if stopped.is_set(): return
            stopped.set()
            grouped = os.name == 'posix' and hasattr(process, 'pid')
            try:
                if grouped: os.killpg(process.pid, signal.SIGTERM)
                elif process.poll() is None: process.terminate()
            except OSError:
                pass
            try: process.wait(timeout=1)
            except subprocess.TimeoutExpired: pass
            try:
                # A plugin may still retain the pipe after its CLI parent exits.
                if grouped: os.killpg(process.pid, signal.SIGKILL)
                elif process.poll() is None: process.kill()
            except OSError:
                pass
            try: process.wait(timeout=1)
            except subprocess.TimeoutExpired: pass
        def timeout():
            expired.set()
            stop()
        timer = threading.Timer(_PREPARATION_LOG_TIMEOUT_SECONDS, timeout)
        timer.daemon = True
        deadline = time.monotonic() + _PREPARATION_LOG_TIMEOUT_SECONDS
        timer.start()
        complete = False
        try:
            records, pending, scanned = [], b'', 0
            while chunk := process.stdout.read(65536):
                scanned += len(chunk)
                if expired.is_set() or scanned > _PREPARATION_LOG_MAX_BYTES:
                    return 'notObserved'
                pieces = (pending + chunk).split(b'\n')
                pending = pieces.pop()
                if len(pending) > _PREPARATION_LOG_MAX_LINE_BYTES:
                    return 'notObserved'
                for line in pieces:
                    record = _preparation_record(line, jobs)
                    if record is not None: records.append(record)
                    if len(records) > _PREPARATION_LOG_MAX_RECORDS:
                        return 'notObserved'
            if pending:
                record = _preparation_record(pending, jobs)
                if record is not None: records.append(record)
            if (len(records) > _PREPARATION_LOG_MAX_RECORDS or expired.is_set()
                    or process.wait(timeout=max(.01, deadline - time.monotonic())) != 0):
                return 'notObserved'
            complete = True
            return {'schema': 'runsc.shared.preparation_timing.v1', 'records': records} if records else 'notObserved'
        finally:
            timer.cancel()
            timer.join()
            if not complete: stop()
            process.stdout.close()
    except Exception:
        # Never retain command stderr, arbitrary log text, response bodies or error strings.
        return 'notObserved'


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


def _admitted_jobs(admitted):
    if not isinstance(admitted, list) or not 1 <= len(admitted) <= 64 * 20:
        raise ValueError('exact admitted Job bindings required')
    jobs = {}
    for record in admitted:
        if (not isinstance(record, dict) or any(not isinstance(record.get(key), str) or not record[key]
                for key in ('agentRunId', 'sessionId'))):
            raise ValueError('original Job Run and Session binding required')
        job_id = 'agent_run.lifecycle:' + record['agentRunId']
        if job_id in jobs:
            raise ValueError('distinct original Job bindings required')
        jobs[job_id] = {key: record[key] for key in ('agentRunId', 'sessionId')}
    return jobs


def validate_job_evidence(evidence, admitted, *, allow_unsettled=False):
    jobs = _admitted_jobs(admitted)
    final = evidence.get('finalJobs')
    if not isinstance(final, list) or len(final) != len(jobs):
        raise ValueError('exact final Job cardinality required')
    seen = set()
    statuses = {'succeeded', 'queued', 'leased', 'running', 'failed', 'dead_lettered', 'cancelled'} if allow_unsettled else {'succeeded'}
    for row in final:
        binding = jobs.get(row.get('jobId')) if isinstance(row, dict) else None
        if (binding is None or row['jobId'] in seen
                or any(row.get(key) != binding[key] for key in ('agentRunId', 'sessionId'))
                or row.get('jobKind') != 'agent_run.lifecycle' or row.get('status') not in statuses
                or type(row.get('retryCount')) is not int or type(row.get('maxRetries')) is not int
                or not 0 <= row['retryCount'] <= row['maxRetries'] < 2**32):
            raise ValueError('successful original Job binding and bounded retry counts required')
        seen.add(row['jobId'])
    yields, claims = evidence.get('jobYields'), evidence.get('capacityClaims')
    if not isinstance(yields, list) or not isinstance(claims, list):
        raise ValueError('durable Job yield and claim evidence required')
    yielded = set()
    for row in yields:
        binding = jobs.get(row.get('jobId')) if isinstance(row, dict) else None
        if (binding is None or any(row.get(key) != binding[key] for key in ('agentRunId', 'sessionId'))
                or type(row.get('yieldedAtMs')) is not int or type(row.get('runAtMs')) is not int
                or not 0 <= row['yieldedAtMs'] <= row['runAtMs']):
            raise ValueError('durable Job yield binding and time required')
        if row.get('transitionReason') not in _YIELD_REASONS:
            raise ValueError('unsupported durable Job yield reason')
        identity = (row['jobId'], row['yieldedAtMs'], row['runAtMs'], row['transitionReason'])
        if identity in yielded:
            raise ValueError('distinct durable Job yields required')
        yielded.add(identity)
    for row in claims:
        binding = jobs.get(row.get('jobId')) if isinstance(row, dict) else None
        if binding is None or row.get('agentRunId') != binding['agentRunId']:
            raise ValueError('original Job capacity claim binding required')
    return {'expectedJobs': len(jobs), 'succeededJobs': sum(row['status'] == 'succeeded' for row in final),
            'jobStatusCounts': dict(sorted(Counter(row['status'] for row in final).items())),
            'failedRetryCount': sum(row['retryCount'] for row in final),
            'jobsWithFailedRetries': sum(row['retryCount'] > 0 for row in final),
            'capacityClaimCount': len(claims), 'yieldCount': len(yields),
            'yieldReasonCounts': dict(sorted(Counter(row['transitionReason'] for row in yields).items()))}


def run(fixture, result, k6binary, *, environments=4, iterations=3, work_units=2000,
        between_run_ms=6000, model_wait_ms=0, initial_stagger_ms=0,
        expected_working_environments=None, run_terminal_timeout_seconds=120, memory_mib=0, minimum_hold_ms=0, execution_mode='perRun', dependencies=None):
    if execution_mode not in ('perRun','retained'):raise ValueError('supported explicit execution mode required')
    profile = memory_profile(environments, iterations, memory_mib, minimum_hold_ms)
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
    budget = workload_budget(environments, iterations, run_terminal_timeout_seconds, between_run_ms, initial_stagger_ms,execution_mode=execution_mode)
    dependencies = dependencies or _dependencies(execution_mode)
    fixture, result = Path(fixture), Path(result)
    driver = dependencies['driver'](fixture, result, expected_running_limit=environments)
    if (driver.manifest.get('sharedWorkload', {}).get('environmentCount') != environments
            or driver.manifest.get('sharedWorkload', {}).get('modelWaitMs',0) != model_wait_ms):
        raise RuntimeError('shared fixture environment count mismatch')
    if driver.manifest.get('executionMode')!=execution_mode:raise RuntimeError('shared fixture execution mode mismatch')
    fixture_profile = driver.manifest.get('sharedWorkload', {}).get('workProfile')
    if (not isinstance(fixture_profile, dict) or set(fixture_profile) != set(profile)
            or any(type(fixture_profile[key]) is not int for key in profile) or fixture_profile != profile):
        raise RuntimeError('shared fixture work profile mismatch')
    report = {'schema': 'runsc.shared.workload.acceptance.v1', 'status': 'failedPreserved',
              'sourceRevision': driver.manifest['sourceRevision'], 'environments': environments, 'executionMode':execution_mode,
              'budgets': budget, 'iterations': iterations, 'runTerminalTimeoutSeconds':run_terminal_timeout_seconds, 'workUnits': work_units, 'betweenRunMs': between_run_ms,
              'modelWaitMs':model_wait_ms,'initialStaggerMs':initial_stagger_ms,'workProfile':profile,
              'initialStaggerTotalMs':initial_stagger_ms*(environments-1),'expectedWorkingEnvironments':expected_working,
              'memoryContinuity': 'notObserved', 'clientFirstToolLatency': 'notObserved',
              'resumeToFirstToolLatency': 'notObserved', 'durableTiming': 'notObserved', 'preparationTiming': 'notObserved',
              'runtimeJobs': 'notObserved', 'memoryContention': 'notObserved',
              'retirement': [], 'perRunCleanup':[], 'responsibility': 'preserved'}
    samples = [{'sampleId': uuid.uuid4().hex, 'operationId': uuid.uuid4().hex, 'workUnits': work_units}
               for _ in range(environments * iterations)]
    if profile['memoryMiB']:
        for sample in samples: sample.update(profile)
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
                    environments=environments, iterations=iterations, timeout=budget['k6ProcessSeconds'], between_run_ms=between_run_ms, initial_stagger_ms=initial_stagger_ms, run_terminal_timeout_seconds=run_terminal_timeout_seconds,execution_mode=execution_mode)
        report['client'] = client
        report['clientFirstToolLatency'] = client.get('firstSuccessfulToolResultLatency', 'notObserved')
        if client.get('status') != 'clientVerified':
            raise RuntimeError('K6 did not verify the full workload')
        records = [json.loads(line) for line in (result / 'k6/run-records.jsonl').read_text().splitlines() if line]
        admitted = [record for record in records if record.get('phase') == 'accepted']
        rows = [driver.completed(record) for record in admitted]
        report['runs'] = rows
        if execution_mode=='retained':report.update(validate_run_facts(rows,environments,iterations))
        else:
            from runsc_acceptance.per_run import validate_runs
            report.update(validate_runs(rows,environments,iterations))
        def observe_job_completion():
            report['durableTiming'] = _durable_timing(driver, admitted)
            summary = validate_job_evidence(report['durableTiming'], admitted, allow_unsettled=True)
            report['runtimeJobs'] = summary
            if summary['failedRetryCount']:
                raise RuntimeError('normal workload consumed the failed retry budget')
            if set(summary['jobStatusCounts']) - {'succeeded', 'leased', 'running'}:
                raise ValueError('original Worker Job ended or queued without successful completion')
            return summary
        driver.wait(observe_job_completion, lambda summary: summary['succeededJobs'] == summary['expectedJobs'],
                    'original Worker Job completion', timeout=FINAL_JOB_SETTLE_SECONDS)
        final=[]
        if execution_mode=='retained':
            last = {record['sessionId']: record for record in admitted}
            final = [driver.wait(lambda label=str(index), run=record: driver.observe(label, run),
                                lambda row: row.get('paused') is True and row.get('powerState') == 'paused',
                                'final exact environment automatic pause')
                     for index, record in enumerate(last.values())]
        report['native'] = observer.stop(); stopped = True
        if (report['native'].get('physicalOverlap') != 'verified'
                or report['native'].get('peakWorkingEnvironments', 0) < expected_working):
            raise RuntimeError('explicit working-environment overlap threshold was not observed')
        if execution_mode=='retained' and iterations > 1 and (report['native'].get('pausedEnvironmentInstances', 0) < environments
                or report['native'].get('resumedEnvironmentInstances', 0) < environments):
            raise RuntimeError('every original environment must be physically observed paused and resumed')
        expected = {row['environmentId']: row['instanceId'] for row in rows}
        for row in final:
            if (expected.get(row.get('environmentId')) != row.get('instanceId')
                    or row.get('runtime') != 'runsc' or row.get('running') is not True):
                raise RuntimeError('final physical environment identity unavailable')
        report['finalEnvironments'] = final
        if execution_mode=='perRun':
            from runsc_acceptance.per_run import validate_cleanup
            for record in admitted:report['perRunCleanup'].append(driver.per_run_cleanup(record))
            report.update(validate_cleanup(report['perRunCleanup'],rows))
            if not driver.per_run_inventory_absent():raise RuntimeError('perRun fixture still has managed physical responsibility')
        if profile['memoryMiB']:
            evaluator = dependencies.get('memory_contention')
            if evaluator is None:
                from runsc_acceptance.memory_contention import evaluate
                evaluator = evaluate
            observations = [json.loads(line) for line in (result / 'native-observations.jsonl').read_text().splitlines() if line]
            cleanup_arguments={'per_run_cleanup':report['perRunCleanup']} if execution_mode=='perRun' else {}
            proof = evaluator(observations, report['durableTiming'], rows, driver.manifest.get('resourceLimits'),**cleanup_arguments)
            if not isinstance(proof, dict) or proof.get('status') != 'verified':
                raise RuntimeError('verified memory contention evidence required')
            report['memoryContention'] = proof
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
        report['preparationTiming'] = _preparation_timing(driver, result / 'k6/run-records.jsonl')
        report['wallTimeMs'] = int((time.monotonic() - began) * 1000)
        (result / 'shared-report.json').write_text(json.dumps(report, indent=2))


def _dependencies(execution_mode):
    source = Path(__file__).resolve().parents[3]
    def load(name, path):
        spec = importlib.util.spec_from_file_location(name, path)
        module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
        return module
    workload = load('shared_k6_workload', source / 'perf/harness/fixture_workload.py')
    observer = load('shared_native_observer', source / 'perf/harness/native_observer.py')
    from runsc_acceptance.per_run import SharedFixtureDriver
    driver=SharedFixtureDriver
    if execution_mode=='perRun':
        from runsc_acceptance.per_run import PerRunFixtureDriver
        driver=PerRunFixtureDriver
    return {'driver': driver, 'workload': workload.run, 'observer': observer.NativeObserver}


def _durable_timing(driver, admitted):
    # One read-only snapshot joins original Jobs, safe yield fields and timings.
    # Model content, error payloads and lease authority never enter the report.
    bindings = _admitted_jobs(admitted)
    code = '''import json
from django.conf import settings
from django.db import connection,transaction
from app_core.models import AgentRun,AgentRunEnvironmentRelease,SessionEvent
assert settings.DATABASES['default']['NAME']=='retained_acceptance'
bindings=JOB_BINDINGS
allowed_yield_reasons=YIELD_REASONS
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
  jobs={job_id:binding['agentRunId'] for job_id,binding in bindings.items()}
  cursor.execute('SELECT job_id,job_kind,status,retry_count,max_retries,session_id FROM runtime.runtime_jobs WHERE job_id=ANY(%s) ORDER BY job_id',[list(jobs)])
  final_jobs=[]
  for job_id,kind,status,retries,maximum,session_id in cursor.fetchall():
   if job_id not in bindings:
    raise RuntimeError('final Job identity invalid')
   final_jobs.append({'agentRunId':jobs[job_id],'jobId':job_id,'jobKind':kind,'status':status,'retryCount':retries,'maxRetries':maximum,'sessionId':session_id})
  cursor.execute('SELECT task_id,session_id,at_ms,payload_json FROM runtime.runtime_events WHERE task_id=ANY(%s) AND event_type=%s ORDER BY at_ms,event_id',[list(jobs),'runtime_job_yielded'])
  yields=[]
  for job_id,session_id,at_ms,payload in cursor.fetchall():
   try:event=json.loads(payload) if isinstance(payload,str) else payload
   except (ValueError,TypeError):raise RuntimeError('durable Job yield payload invalid') from None
   if not isinstance(event,dict):raise RuntimeError('durable Job yield payload invalid')
   yielded,run_at=event.get('yieldedAtMs'),event.get('runAtMs')
   if (job_id not in bindings or bindings[job_id]['sessionId']!=session_id or event.get('schema')!='runtime.job.yielded.v1' or event.get('jobId')!=job_id or type(at_ms) is not int or type(yielded) is not int or type(run_at) is not int or not 0<=yielded<=run_at or at_ms!=yielded):
    raise RuntimeError('durable Job yield identity or time invalid')
   reason=event.get('transitionReason')
   safe_reason=reason if isinstance(reason,str) and reason in allowed_yield_reasons else 'unrecognized'
   yields.append({'agentRunId':jobs[job_id],'jobId':job_id,'sessionId':session_id,'yieldedAtMs':yielded,'runAtMs':run_at,'transitionReason':safe_reason})
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
  print(json.dumps({'schema':'runsc.shared.durable_timing.v1','toolTimingBasis':'committedSessionEvent.createdAtMs','rows':rows,'finalJobs':final_jobs,'jobYields':yields,'capacityClaims':claims,'releaseToClaimMs':physical if physical else 'notObserved','hostPreparationToClaimMs':preparation if preparation else 'notObserved','clientFirstToolLatency':'notObserved','resumeToFirstToolLatency':'notObserved'}))
'''.replace('RUN_IDS', repr([record['agentRunId'] for record in admitted])).replace('JOB_BINDINGS', repr(bindings)).replace('YIELD_REASONS', repr(sorted(_YIELD_REASONS)))
    return driver.sql(code)


def main():
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fixture', required=True, type=Path)
    parser.add_argument('--execution-mode',choices=('perRun','retained'),default='perRun')
    parser.add_argument('--result', required=True, type=Path)
    parser.add_argument('--k6-binary', required=True, type=Path)
    parser.add_argument('--environment-count', type=int, default=4)
    parser.add_argument('--iterations', type=int, default=3)
    parser.add_argument('--work-units', type=int, default=2000)
    parser.add_argument('--memory-mib', type=int, default=0)
    parser.add_argument('--minimum-hold-ms', type=int, default=0)
    parser.add_argument('--between-run-ms', type=int, default=6000)
    parser.add_argument('--model-wait-ms', type=int, default=0)
    parser.add_argument('--initial-stagger-ms', type=int, default=0)
    parser.add_argument('--run-terminal-timeout-seconds', type=int, default=120)
    parser.add_argument('--expected-working-environments', type=int)
    args = parser.parse_args()
    report = run(args.fixture, args.result, args.k6_binary, environments=args.environment_count,
                 iterations=args.iterations, work_units=args.work_units, between_run_ms=args.between_run_ms,
                 memory_mib=args.memory_mib,minimum_hold_ms=args.minimum_hold_ms,execution_mode=args.execution_mode,
                 model_wait_ms=args.model_wait_ms,initial_stagger_ms=args.initial_stagger_ms,
                 expected_working_environments=args.expected_working_environments,run_terminal_timeout_seconds=args.run_terminal_timeout_seconds)
    print(json.dumps(report))


if __name__ == '__main__': main()
