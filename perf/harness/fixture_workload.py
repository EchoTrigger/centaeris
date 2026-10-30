"""Run existing K6 workloads against one already-owned native fixture; no deployment."""
import importlib.util
import json
import math
import os
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[2]


def _module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


fixed_workload = _module('perf_fixed_workload', ROOT / 'perf/mock-model/fixed_workload.py')
workload_budget = _module('perf_workload_budget', ROOT / 'scripts/workspace/runsc_acceptance/workload_budget.py').workload_budget
fixture_execution_mode = _module('perf_execution_mode', ROOT / 'scripts/workspace/runsc_acceptance/execution_mode.py').fixture_execution_mode


def _external(path):
    value = Path(path).resolve()
    if value.is_relative_to(ROOT.resolve()):
        raise ValueError('private fixture/output must be outside the source checkout')
    return value


def verify_native_fixture(path):
    path = _external(path)
    native = json.loads(path.read_text(encoding='utf-8'))
    isolation = _module('perf_native_isolation', ROOT / 'scripts/workspace/runsc_acceptance/isolation.py')
    owner_path = Path(native['isolationOwner']).resolve(strict=True)
    owner = isolation.verify_owner(owner_path)
    compose = json.loads(path.with_name('compose.json').read_text(encoding='utf-8'))
    if (os.name != 'posix' or Path('/proc/self/ns/net').stat().st_ino
            != Path('/proc/' + str(owner['daemonPid']) + '/ns/net').stat().st_ino):
        raise ValueError('K6 must run inside the owned Linux fixture network namespace')
    if (path.parent.parent != Path(owner['root']).resolve() or owner_path != path.parent.parent / 'owner.json'
            or native['project'] != owner.get('project') or compose['name'] != native['project']
            or native['sourceRevision'] != owner.get('sourceRevision')
            or Path(native['source']).resolve() != ROOT.resolve()
            or not re.fullmatch('retainedaccept[a-f0-9]{10}', native['project'])
            or type(native['apiPort']) is not int or not 1024 <= native['apiPort'] <= 65535
            or compose['services']['api']['ports'] != [f"127.0.0.1:{native['apiPort']}:8000"]):
        raise ValueError('native fixture ownership or API binding mismatch')
    mode=fixture_execution_mode(native,compose)
    model_wait_ms = (native.get('sharedWorkload') or {}).get('modelWaitMs', 0)
    if type(model_wait_ms) is not int or not 0 <= model_wait_ms <= 120000:
        raise ValueError('native fixture model wait is invalid')
    return {'apiBase': f"http://127.0.0.1:{native['apiPort']}", 'project': native['project'],
            'sourceRevision': native['sourceRevision'], 'modelWaitMs': model_wait_ms,'executionMode':mode}


def read_fixture(path, *, environments, iterations):
    path = _external(path)
    if path.is_symlink() or path.stat().st_size > 1024 * 1024:
        raise ValueError('bounded private fixture file required')
    if os.name == 'posix' and path.stat().st_mode & 0o077:
        raise ValueError('fixture credentials file must be mode 0600')
    value = json.loads(path.read_text(encoding='utf-8'))
    fields = {'schema', 'nativeManifest', 'apiBase', 'workspaceId', 'agentId', 'modelRefs', 'account', 'samples'}
    identity = lambda item: isinstance(item, str) and re.fullmatch('[A-Za-z0-9_.:-]{1,160}', item)
    if (not isinstance(value, dict) or set(value) != fields or value['schema'] != 'workspace.perf.fixture.v1'
            or not identity(value['workspaceId']) or not identity(value['agentId'])
            or not isinstance(value['modelRefs'], dict) or 'tools' not in value['modelRefs']
            or set(value['modelRefs']) - {'short', 'tools', 'long'}
            or any(not identity(item) for item in value['modelRefs'].values())
            or not isinstance(value['account'], dict) or set(value['account']) != {'email', 'password'}
            or any(not isinstance(item, str) or not 1 <= len(item) <= 512 for item in value['account'].values())
            or type(environments) is not int or not 1 <= environments <= 64 or type(iterations) is not int or not 1 <= iterations <= 20
            or not isinstance(value['samples'], list) or len(value['samples']) != environments * iterations):
        raise ValueError('explicit owned workload fixture invalid')
    samples = [fixed_workload.validate_sample(sample) for sample in value['samples']]
    if (len({sample['sampleId'] for sample in samples}) != len(samples)
            or len({sample['operationId'] for sample in samples}) != len(samples)
            or len({sample['workUnits'] for sample in samples}) != 1):
        raise ValueError('fixed samples must be distinct with identical work units')
    native = verify_native_fixture(value['nativeManifest'])
    if value['apiBase'] != native['apiBase']:
        raise ValueError('workload API does not match the owned native fixture')
    return {**value, 'native': native}


def prepared_samples(fixture):
    import hashlib
    samples = []
    for sample in fixture['samples']:
        samples.append({**sample, 'expectedArtifact': fixed_workload.expected_artifact(sample),
                        'expectedCommandSha256': 'sha256:' + hashlib.sha256(fixed_workload.command_for(sample).encode()).hexdigest()})
    return samples


def k6_command(fixture, output, binary, *, environments, iterations, between_run_ms=0, initial_stagger_ms=0, run_terminal_timeout_seconds=120,execution_mode='perRun'):
    if type(run_terminal_timeout_seconds) is not int or not 1 <= run_terminal_timeout_seconds <= 600:
        raise ValueError('bounded per-Run terminal timeout required')
    if (type(initial_stagger_ms) is not int or not 0 <= initial_stagger_ms <= 120000
            or initial_stagger_ms * (environments - 1) > 120000):
        raise ValueError('bounded initial workload stagger required')
    if type(between_run_ms) is not int or not 0 <= between_run_ms <= 60000:
        raise ValueError('bounded between-Run gap required')
    budget = workload_budget(environments, iterations, run_terminal_timeout_seconds, between_run_ms, initial_stagger_ms,execution_mode=execution_mode)
    allowed = {'PATH', 'SYSTEMROOT', 'WINDIR', 'COMSPEC', 'TEMP', 'TMP', 'HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA'}
    environment = {key: value for key, value in os.environ.items() if key.upper() in allowed}
    environment.update(API_BASE=fixture['apiBase'], PERF_WORKSPACE_ID=fixture['workspaceId'],
                       PERF_AGENT_ID=fixture['agentId'], PERF_MODEL_REFS=json.dumps(fixture['modelRefs']),
                       PERF_ADMIN_EMAIL=fixture['account']['email'], PERF_ADMIN_PASSWORD=fixture['account']['password'],
                       PERF_SAMPLES_PATH=str(Path(output) / 'samples.json'), PERF_ENVIRONMENT_COUNT=str(environments),
                       PERF_K6_BUDGET=json.dumps(budget['k6']), PERF_RUNS_PER_ENVIRONMENT=str(iterations), PERF_BETWEEN_RUN_MS=str(between_run_ms), PERF_INITIAL_STAGGER_MS=str(initial_stagger_ms), PERF_RUN_TERMINAL_TIMEOUT_SECONDS=str(run_terminal_timeout_seconds), K6_NO_USAGE_REPORT='true')
    command = [str(binary), '--log-format', 'json', 'run', '--summary-export', str(Path(output) / 'k6-summary.json'),
               str(ROOT / 'perf/k6/scenarios/multi-environment-workload.js')]
    return command, environment


def console_record(line):
    try:
        envelope = json.loads(line)
    except ValueError:
        if 'PERF_RUN ' in line:
            raise ValueError('malformed K6 sample record') from None
        return None
    message = envelope.get('msg') if isinstance(envelope, dict) else None
    if not isinstance(message, str) or not message.startswith('PERF_RUN '):
        return None
    value = json.loads(message[len('PERF_RUN '):])
    if (not isinstance(value, dict) or value.get('schema') != 'workspace.perf.run.sample.v1'
            or value.get('phase') not in {'accepted', 'verified', 'failed', 'notSubmitted'}):
        raise ValueError('K6 sample record schema invalid')
    return value


def validate_records(records, fixture):
    samples = {sample['sampleId']: sample for sample in prepared_samples(fixture)}
    accepted, verified = {}, {}
    for record in records:
        sample = samples.get(record.get('sampleId'))
        if (sample is None or record.get('operationId') != sample['operationId']
                or record.get('workspaceId') != fixture['workspaceId']
                or record.get('modelConfigRef') != fixture['modelRefs']['tools']):
            raise ValueError('K6 sample target binding invalid')
        phase = record['phase']
        if phase == 'accepted':
            if record['sampleId'] in accepted or not record.get('agentRunId') or not record.get('sessionId'):
                raise ValueError('K6 admitted Run identity invalid')
            accepted[record['sampleId']] = record
        elif phase == 'verified':
            previous = accepted.get(record['sampleId'])
            artifact = record.get('artifact', {})
            if (previous is None or record['sampleId'] in verified
                    or any(record.get(key) != previous.get(key) for key in ('agentRunId', 'sessionId'))
                    or artifact.get('sha256') != sample['expectedArtifact']['sha256']
                    or artifact.get('sizeBytes') != sample['expectedArtifact']['sizeBytes']):
                raise ValueError('K6 verified artifact/Run binding invalid')
            verified[record['sampleId']] = record
    if len({record['agentRunId'] for record in accepted.values()}) != len(accepted):
        raise ValueError('distinct actual Run identities required')
    if len(accepted) != len(samples) or len(verified) != len(samples) or any(
            record['phase'] in ('failed', 'notSubmitted') for record in records):
        raise ValueError('K6 workload did not verify all planned samples')
    measured = [record['firstSuccessfulToolResultMs'] for record in verified.values()
                if 'firstSuccessfulToolResultMs' in record]
    if any(type(value) not in (int, float) or not math.isfinite(value) or value < 0 for value in measured):
        raise ValueError('actual finite client tool receive times required')
    timing = {'source': 'k6SseReceive', 'valuesMs': measured} if len(measured) == len(samples) else 'notObserved'
    return {'plannedSamples': len(samples), 'acceptedSamples': len(accepted), 'verifiedSamples': len(verified),
            'acceptedRunIds': sorted(record['agentRunId'] for record in accepted.values()),
            'firstSuccessfulToolResultLatency': timing}


def run(fixture_path, output, binary, *, environments=4, iterations=3, timeout=None, between_run_ms=0, initial_stagger_ms=0, run_terminal_timeout_seconds=120,execution_mode='perRun'):
    import queue
    import subprocess
    import threading
    import time
    if type(run_terminal_timeout_seconds) is not int or not 1 <= run_terminal_timeout_seconds <= 600:
        raise ValueError('bounded per-Run terminal timeout required')
    budget = workload_budget(environments, iterations, run_terminal_timeout_seconds, between_run_ms, initial_stagger_ms,execution_mode=execution_mode)
    if timeout is None:
        timeout = budget['k6ProcessSeconds']
    if type(timeout) is not int or timeout != budget['k6ProcessSeconds']:
        raise ValueError('K6 process budget must equal the recorded derived budget')
    fixture = read_fixture(fixture_path, environments=environments, iterations=iterations)
    if fixture['native']['executionMode']!=execution_mode:raise ValueError('workload execution mode mismatch')
    output = _external(output)
    binary = Path(binary).resolve(strict=True)
    with binary.open('rb') as handle:
        if handle.read(6) != b'\x7fELF\x02\x01':
            raise ValueError('owned Linux fixture requires Linux ELF64 K6, not Windows interoperability')
    output.mkdir(mode=0o700, parents=False, exist_ok=False)
    samples = prepared_samples(fixture)
    (output / 'samples.json').write_text(json.dumps(samples, indent=2), encoding='utf-8')
    manifest = {key: fixture[key] for key in ('apiBase', 'workspaceId', 'agentId', 'modelRefs')}
    manifest.update(schema='workspace.perf.workload.v1', budgets=budget, environments=environments, iterations=iterations,
                    sourceRevision=fixture['native']['sourceRevision'], project=fixture['native']['project'], executionMode=execution_mode, betweenRunMs=between_run_ms,
                    initialStaggerMs=initial_stagger_ms, runTerminalTimeoutSeconds=run_terminal_timeout_seconds, modelWaitMs=fixture['native'].get('modelWaitMs', 0))
    (output / 'manifest.json').write_text(json.dumps(manifest, indent=2), encoding='utf-8')
    command, environment = k6_command(fixture, output, binary, environments=environments, iterations=iterations, between_run_ms=between_run_ms, initial_stagger_ms=initial_stagger_ms, run_terminal_timeout_seconds=run_terminal_timeout_seconds,execution_mode=execution_mode)
    started = time.monotonic()
    process = subprocess.Popen(command, env=environment, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, text=True, encoding='utf-8')
    lines = queue.Queue()
    def read_lines():
        try:
            for line in process.stdout:
                lines.put(line)
        finally:
            lines.put(None)
    reader = threading.Thread(target=read_lines, daemon=True); reader.start()
    records = []; report = {**manifest, 'status': 'clientFailed', 'physicalOverlap': 'notObserved',
                           'durableTerminalAndCleanup': 'notObserved', 'retirement': 'notObserved'}
    deadline = time.monotonic() + timeout
    try:
        with (output / 'k6.log').open('x', encoding='utf-8') as log, (output / 'run-records.jsonl').open('x', encoding='utf-8') as ledger:
            while True:
                if time.monotonic() >= deadline:
                    raise TimeoutError('K6 workload deadline reached; product Run responsibility remains with fixture owner')
                try:
                    line = lines.get(timeout=min(.5, deadline - time.monotonic()))
                except queue.Empty:
                    continue
                if line is None:
                    break
                line = line.replace(fixture['account']['password'], '[redacted]')
                log.write(line); log.flush()
                record = console_record(line)
                if record is not None:
                    records.append(record); ledger.write(json.dumps(record) + '\n'); ledger.flush()
        process.wait(timeout=max(1, deadline - time.monotonic()))
        if process.returncode:
            raise RuntimeError('K6 process failed; see redacted external log')
        report.update(validate_records(records, fixture), status='clientVerified')
        return report
    except BaseException as error:
        report['errorType'] = type(error).__name__
        raise
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill(); process.wait(timeout=10)
        reader.join(timeout=5)
        report['wallTimeMs'] = (time.monotonic() - started) * 1000
        (output / 'client-report.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
