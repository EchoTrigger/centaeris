"""One synthetic Session: fresh-instance restore, Runtime restart and reachable-head GC.

Admission uses the public fixture driver; k6 independently verifies committed tools
and artifact bytes. Only the already-owned fixture may be restarted or collected.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sys
import time

discovery_root = str(Path(__file__).resolve().parents[1])
if discovery_root not in sys.path:
    sys.path.insert(0, discovery_root)
from runsc_acceptance.per_run import PerRunFixtureDriver, validate_cleanup

SOURCE = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location('recovery_fixed_workload', SOURCE / 'perf/mock-model/fixed_workload.py')
workload = importlib.util.module_from_spec(spec)
spec.loader.exec_module(workload)


class RecoveryFixtureDriver(PerRunFixtureDriver):
    def restart(self):
        result = super().restart()
        code = """import json
from django.conf import settings
from app_core.models import Session
from app_core.workspace_snapshot_roots import request_workspace_snapshot_roots,WorkspaceRootsUnavailable
assert settings.DATABASES['default']['NAME']=='retained_acceptance'
assert Session.objects.count()==1
try:
 request_workspace_snapshot_roots(Session.objects.get(pk=SESSION_ID))
 ready=True
except WorkspaceRootsUnavailable:ready=False
print(json.dumps({'rootsReady':ready}))
""".replace('SESSION_ID', repr(self.sessions['workspace-recovery']))
        self.wait(lambda: self.sql(code), lambda value: value['rootsReady'], 'restarted Runtime recovery roots ready')
        return {**result, 'rootsReady': True}

    def settle(self, admitted):
        code = """import json
from django.conf import settings
from django.db import connection
assert settings.DATABASES['default']['NAME']=='retained_acceptance'
with connection.cursor() as cursor:
 cursor.execute('SELECT job_id,status,retry_count FROM runtime.runtime_jobs WHERE job_id=%s AND session_id=%s',
  ['agent_run.lifecycle:'+RUN_ID,SESSION_ID])
 row=cursor.fetchone()
 if row is None:raise RuntimeError('original canonical Job missing')
 print(json.dumps(dict(zip(['jobId','status','retryCount'],row))))
""".replace('RUN_ID', repr(admitted['agentRunId'])).replace('SESSION_ID', repr(admitted['sessionId']))
        def observe():
            value = self.sql(code)
            if value['retryCount'] != 0 or value['status'] not in ('queued', 'leased', 'running', 'succeeded'):
                raise RuntimeError('normal recovery gate consumed failure budget or lost canonical Job')
            return value
        return self.wait(observe, lambda value: value['status'] == 'succeeded', 'original canonical Job settled')

    def head(self, session_id):
        code = """import hashlib,json
from django.conf import settings
from django.core.files.storage import default_storage
from django.db import transaction
from app_core.models import Session
assert settings.DATABASES['default']['NAME']=='retained_acceptance'
with transaction.atomic():
 session=Session.objects.get(pk=SESSION_ID,workspace_id=WORKSPACE_ID)
 value={'baselineKnown':session.workspaceBaselineKnown,'generation':session.workspaceGeneration,
  'storageKey':session.workspaceStorageKey,'snapshotSha256':session.workspaceSnapshotSha256,
  'snapshotEncoding':session.workspaceSnapshotEncoding,'snapshotSizeBytes':session.workspaceSnapshotSizeBytes,
  'expandedSizeBytes':session.workspaceExpandedSizeBytes,'fileCount':session.workspaceFileCount}
 if value['baselineKnown'] is not True or value['generation']<1:raise RuntimeError('known committed nonempty head required')
 expected=f"workspaces/{session.workspace_id}/sessions/{session.pk}/snapshots/{value['generation']}/{value['snapshotSha256'][7:]}.snapshot"
 if value['storageKey']!=expected or not 0<value['snapshotSizeBytes']<=8*1024**2:raise RuntimeError('bounded canonical head required')
 with default_storage.open(expected,'rb') as stream:data=stream.read(8*1024**2+1)
 if len(data)!=value['snapshotSizeBytes']:raise RuntimeError('actual head size mismatch')
 value['payloadSha256']='sha256:'+hashlib.sha256(data).hexdigest()
 if value['payloadSha256']!=value['snapshotSha256']:raise RuntimeError('actual head digest mismatch')
 print(json.dumps(value))
""".replace('SESSION_ID', repr(session_id)).replace('WORKSPACE_ID', repr(self.seed['workspaceId']))
        return self.sql(code)

    def collect(self, old, current):
        # Real grace/cutoff, with no timestamp rewrites or fabricated garbage.
        time.sleep(2)
        code = """import json
from datetime import timedelta
from django.conf import settings
from django.core.files.storage import default_storage
from django.utils import timezone
from app_core.models import Session
from app_core.workspace_snapshot_gc import collect_workspace_snapshot_gc
assert settings.DATABASES['default']['NAME']=='retained_acceptance'
assert Session.objects.count()==1,'single owned synthetic Session required'
old,current=OLD_KEY,CURRENT_KEY
cutoff=timezone.now()-timedelta(seconds=1)
planned=collect_workspace_snapshot_gc(cutoff,True)
if planned.failures or old not in planned.planned:raise RuntimeError('old head not eligible for real GC')
actual=collect_workspace_snapshot_gc(cutoff,False)
if actual.failures or old not in actual.cleaned or default_storage.exists(old) or not default_storage.exists(current):
 raise RuntimeError('actual old-head collection/current-head preservation failed')
print(json.dumps({'planned':planned.planned,'cleaned':actual.cleaned,'blocked':actual.blocked,'preserved':current}))
""".replace('OLD_KEY', repr(old['storageKey'])).replace('CURRENT_KEY', repr(current['storageKey']))
        return self.sql(code)


def verify_run(driver, admitted, sample, binary):
    """Run the existing smoke verifier against this exact public admission."""
    phase = sample['phase']
    fixture = {'schema': 'workspace.perf.run.observation.v1', **{key: admitted[key] for key in ('sessionId', 'agentRunId')},
               'expectedCommandSha256': 'sha256:' + hashlib.sha256(workload.recovery_command(sample).encode()).hexdigest(),
               'expectedArtifact': workload.recovery_expectation(sample['caseId'])}
    path = driver.result / (phase + '-observation.json')
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, 'w') as stream:
        json.dump(fixture, stream)
    environment = {key: value for key, value in driver.environment.items()
                   if key.upper() in {'PATH', 'HOME', 'TMP', 'TMPDIR', 'TEMP', 'SYSTEMROOT'}}
    environment.update(API_BASE=driver.base, PERF_WORKSPACE_ID=driver.seed['workspaceId'],
                       PERF_AGENT_ID=driver.seed['agentId'], PERF_MODEL_REFS=json.dumps({'short': driver.seed['modelConfigRef']}),
                       PERF_ADMIN_EMAIL=driver.seed['email'], PERF_ADMIN_PASSWORD=driver.seed['password'],
                       PERF_RUN_FIXTURE_PATH=str(path), K6_NO_USAGE_REPORT='true')
    # Only the allowlisted success record becomes evidence; process diagnostics remain private.
    import subprocess
    result = subprocess.run([str(binary), '--log-format', 'json', 'run', '--summary-export',
                             str(driver.result / (phase + '-k6-summary.json')),
                             str(SOURCE / 'perf/k6/scenarios/run-smoke.js')], env=environment,
                            stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=180)
    if result.returncode != 0:
        raise RuntimeError('k6 recovery phase failed: ' + phase)
    records = []
    for line in result.stdout.splitlines() + result.stderr.splitlines():
        try:
            message = json.loads(line).get('msg', '')
        except (ValueError, AttributeError):
            continue
        if message.startswith('PERF_RECOVERY_RUN '):
            records.append(json.loads(message[len('PERF_RECOVERY_RUN '):]))
    if len(records) != 1:
        raise RuntimeError('one independently verified k6 artifact required')
    record = records[0]
    if (record.get('schema') != 'workspace.perf.run.verified.v1'
            or any(record.get(key) != admitted[key] for key in ('sessionId', 'agentRunId'))
            or any(record.get('artifact', {}).get(key) != value for key, value in fixture['expectedArtifact'].items() if key != 'filename')):
        raise RuntimeError('k6 verification identity or byte evidence mismatch')
    return record


def _known_head(head):
    if (head.get('baselineKnown') is not True or type(head.get('generation')) is not int or head['generation'] < 1
            or not head.get('storageKey') or head.get('snapshotEncoding') != 'fileManifestV1'
            or any(type(head.get(key)) is not int or head[key] < 1 for key in ('snapshotSizeBytes', 'expandedSizeBytes', 'fileCount'))
            or head.get('payloadSha256') != head.get('snapshotSha256')):
        raise RuntimeError('known durable nonempty verified Session head required')


def run(fixture, result, binary, *, dependencies=None):
    dependencies = dependencies or {'driver': RecoveryFixtureDriver, 'verify': verify_run}
    fixture, result = Path(fixture), Path(result)
    manifest = json.loads((fixture / 'manifest.json').read_text()) if (fixture / 'manifest.json').exists() else {}
    driver = dependencies['driver'](fixture, result,
                                    expected_running_limit=manifest.get('sharedWorkload', {}).get('environmentCount', 2))
    if driver.manifest.get('workspaceRecovery') is not True or driver.manifest.get('executionMode') != 'perRun':
        raise RuntimeError('explicit isolated perRun workspace recovery fixture required')
    report = {'schema': 'runsc.workspace.recovery.acceptance.v1', 'status': 'failedPreserved',
              'sourceRevision': driver.manifest['sourceRevision'], 'caseId': driver.tag,
              'runs': [], 'cleanup': [], 'heads': [], 'verifiedArtifacts': [], 'canonicalJobs': []}
    session = None
    instances, containers = set(), set()
    try:
        for phase in ('write', 'readAndAdvance', 'read'):
            sample = {'schema': 'workspace.recovery.fixture.v1', 'caseId': driver.tag, 'phase': phase}
            admitted = driver.admit('workspace-recovery', workload.RECOVERY_MARKER + json.dumps(sample))
            if session is not None and admitted['sessionId'] != session:
                raise RuntimeError('same original public Session required')
            session = admitted['sessionId']
            report['verifiedArtifacts'].append(dependencies['verify'](driver, admitted, sample, binary))
            row = driver.completed(admitted)
            proof = driver.per_run_cleanup(admitted)
            try:
                validate_cleanup([proof], [row])
            except ValueError as error:
                raise RuntimeError('original Run cleanup evidence rejected') from error
            if (row['instanceId'] != proof['instanceId'] or row['instanceId'] in instances
                    or proof['containerId'] in containers or not driver.per_run_inventory_absent()):
                raise RuntimeError('fresh physical instance and exact absence between Runs required')
            instances.add(row['instanceId'])
            containers.add(proof['containerId'])
            report['runs'].append(row)
            report['cleanup'].append(proof)
            report['canonicalJobs'].append(driver.settle(admitted))
            head = driver.head(session)
            _known_head(head)
            report['heads'].append(head)
            if phase == 'readAndAdvance':
                old = report['heads'][0]
                if head['generation'] <= old['generation'] or head['storageKey'] == old['storageKey']:
                    raise RuntimeError('independent churn must advance committed head before GC')
                report['restart'] = driver.restart()
                if (report['restart'].get('performed') is not True
                        or report['restart'].get('beforeRuntimeStartedAt') == report['restart'].get('afterRuntimeStartedAt')
                        or driver.head(session) != head):
                    raise RuntimeError('committed head changed across owned Runtime restart')
                report['gc'] = driver.collect(old, head)
                if driver.head(session) != head:
                    raise RuntimeError('GC changed committed head')
            elif phase == 'read' and head != report['heads'][1]:
                raise RuntimeError('read-only restored workspace advanced or changed head')
        report.update(status='passed', sessionId=session, responsibility='released')
        return report
    except BaseException as error:
        report['errorType'] = type(error).__name__
        raise
    finally:
        (result / 'workspace-recovery-report.json').write_text(json.dumps(report, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fixture', type=Path, required=True)
    parser.add_argument('--result', type=Path, required=True)
    parser.add_argument('--k6-binary', type=Path, required=True)
    args = parser.parse_args()
    binary = args.k6_binary.resolve(strict=True)
    with binary.open('rb') as stream:
        if stream.read(6) != b'\x7fELF\x02\x01':
            parser.error('pinned Linux ELF64 k6 required')
    report = run(args.fixture, args.result, binary)
    print(json.dumps({'status': report['status'], 'completedRuns': len(report['runs']), 'sessionId': report['sessionId']}))


if __name__ == '__main__':
    main()
