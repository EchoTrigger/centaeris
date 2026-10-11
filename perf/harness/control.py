"""Isolated performance deployment control. Never reads the root private .env."""
import argparse
import base64
import datetime
import hashlib
import json
import os
import re
from pathlib import Path
import secrets
import subprocess
import sys
import time

PROJECT = 'centaeris-perf'
ROOT = Path(__file__).resolve().parents[2]
SERVICES = ('api', 'runtime', 'worker', 'postgres', 'redis')


def read_env(path):
    return dict(line.split('=', 1) for line in path.read_text(encoding='utf-8').splitlines()
                if line and not line.startswith('#') and '=' in line)


def initialize(root, oci_runtime=None):
    if oci_runtime is not None and oci_runtime not in {'runc', 'runsc'}:
        raise ValueError('oci_runtime must be exactly runc or runsc')
    values = read_env(root / '.env.example')
    if oci_runtime is not None:
        values['OCI_RUNTIME'] = oci_runtime
    for key in ('DJANGO_SECRET_KEY', 'INTERNAL_API_TOKEN', 'AGENT_RUN_AUTHORIZATION_SIGNING_KEY',
                'POSTGRES_PASSWORD', 'BOOTSTRAP_SUPERADMIN_PASSWORD'):
        values[key] = secrets.token_hex(32)
    values['CREDENTIAL_ENCRYPTION_KEY'] = base64.urlsafe_b64encode(secrets.token_bytes(32)).decode()
    values.update(COMPOSE_PROJECT_NAME=PROJECT, API_HOST_PORT='18000', WEB_HOST_PORT='13000',
                  API_BASE_URL='http://localhost:18000', WEB_ORIGIN='http://localhost:13000',
                  POSTGRES_TEST_HOST_PORT='55433', REDIS_TEST_HOST_PORT='16379',
                  BOOTSTRAP_SUPERADMIN_EMAIL='perf-admin@localhost.invalid',
                  DJANGO_DEBUG='0', PASSWORD_RESET_ENABLED='0', PASSWORD_RESET_MAIL_SENDER='0',
                  WORKER_SLOT_COUNT='2',
                  # Explicit synthetic inputs for this isolated two-slot fixture.
                  # Two overlapping spool copies per 1 MiB upload fit two slots.
                  UPLOAD_BODY_MAX_BYTES='1048576', UPLOAD_TEMP_MAX_BYTES='4194304',
                  UPLOAD_MAX_CONCURRENT='2')
    path = root / 'perf/.state/test.env'
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open('x', encoding='utf-8') as output:
        output.write('\n'.join(f'{key}={value}' for key, value in values.items()) + '\n')
    return path


def compose_command(root, env_file, args, slots=None):
    # An allowlist prevents shell variables from overriding the explicit test env.
    allowed = {'PATH', 'SYSTEMROOT', 'WINDIR', 'COMSPEC', 'TEMP', 'TMP', 'HOME', 'USERPROFILE',
               'APPDATA', 'LOCALAPPDATA', 'PROGRAMDATA', 'PROGRAMFILES', 'PROGRAMFILES(X86)',
               'PROGRAMW6432', 'DOCKER_CONFIG', 'DOCKER_HOST',
               'DOCKER_CONTEXT', 'DOCKER_TLS_VERIFY', 'DOCKER_CERT_PATH'}
    env = {key: value for key, value in os.environ.items() if key.upper() in allowed}
    env['CENTAERIS_SOURCE_REVISION'] = subprocess.check_output(
        ['git', '-C', str(ROOT), 'rev-parse', 'HEAD'], text=True, encoding='utf-8'
    ).strip()
    if slots is not None:
        if type(slots) is not int or not 1 <= slots <= 16:
            raise ValueError('slots must be between 1 and 16')
        env['WORKER_SLOT_COUNT'] = str(slots)
    return ['docker', 'compose', '--project-name', PROJECT, '--project-directory', root.as_posix(),
            '--env-file', env_file.as_posix(), '-f', (root / 'docker-compose.yml').as_posix(),
            '-f', (root / 'perf/compose.perf.yml').as_posix(), *args], env


def validate_config(config, root):
    if config.get('name') != PROJECT:
        raise ValueError('performance project identity mismatch')
    for group in ('volumes', 'networks'):
        for resource in config.get(group, {}).values():
            if resource.get('external') or not resource.get('name', '').startswith(PROJECT + '_'):
                raise ValueError('resource must belong exclusively to the performance project')
    ca = (root / 'perf/certs/out/isolated/mock-ca.crt').as_posix()
    for name, service in config['services'].items():
        if service.get('container_name') or service.get('network_mode') not in (None, 'none'):
            raise ValueError('custom container identity or shared network is forbidden')
        for mount in service.get('volumes', []):
            if mount['type'] == 'volume' and mount['source'] not in config.get('volumes', {}):
                raise ValueError('unowned volume mount')
            if mount['type'] == 'bind':
                source = mount['source'].replace('\\', '/')
                if source == '/var/run/docker.sock' and name in {'runtime', 'material-worker'}:
                    continue
                permitted = {ca, (root / 'perf/certs/out/isolated/mock-model-bundle.pem').as_posix()}
                if source not in permitted or not mount.get('read_only'):
                    raise ValueError('unexpected bind mount')
        for port in service.get('ports', []):
            if port.get('host_ip') != '127.0.0.1' or str(port.get('published')) not in {'18000', '13000', '55433', '16379'}:
                raise ValueError('unexpected published port')
    for name in ('api', 'runtime'):
        service = config['services'][name]
        if service.get('environment', {}).get('SSL_CERT_FILE') != '/perf-certs/mock-ca.crt':
            raise ValueError('mock TLS trust configuration missing')
        if not any(m.get('target') == '/perf-certs/mock-ca.crt' for m in service.get('volumes', [])):
            raise ValueError('mock CA mount missing')
    runtime_env = config['services']['runtime'].get('environment', {})
    for key in ('PLUGIN_VOLUME_NAME', 'AGENT_MEMORY_VOLUME_NAME'):
        if key in runtime_env and not runtime_env[key].startswith(PROJECT + '_'):
            raise ValueError('runtime references a foreign volume')


def run(command, **kwargs):
    result = subprocess.run(command, capture_output=True, text=True, encoding='utf-8',
                            errors='strict', timeout=kwargs.pop('timeout', 120), **kwargs)
    if result.returncode:
        # Compose errors may contain interpolated credentials; keep those out of logs.
        raise RuntimeError(f'command failed (exit {result.returncode}): {command[0]}')
    return result.stdout


def parse_stats(text, expected):
    rows = []
    for line in text.splitlines():
        value = json.loads(line)
        rows.append({'container': value['Name'], 'cpuPercent': float(value['CPUPerc'].removesuffix('%')),
                     'memory': value['MemUsage']})
    if {row['container'] for row in rows} != expected or any(not r['memory'] for r in rows):
        raise ValueError('resource sample is incomplete')
    return rows


def switch_slots(stack, slots):
    if type(slots) is not int or not 1 <= slots <= 16:
        raise ValueError('slots must be between 1 and 16')
    if not stack.quiet():
        raise RuntimeError('stack has active runs; refusing to replace worker')
    before = stack.identities()
    stack.compose(['up', '-d', '--no-deps', '--force-recreate', 'worker'], slots=slots)
    stack.verify_slots(slots)
    if stack.identities() != before:
        raise RuntimeError('a non-worker container changed identity')


def account_runs(accepted, rows, accepted_count=None):
    if accepted_count is not None and accepted_count != len(accepted):
        raise RuntimeError('K6 accepted count does not match captured run identities')
    if not accepted or len(set(accepted)) != len(accepted) or len(rows) != len(accepted) or {r['id'] for r in rows} != set(accepted):
        raise RuntimeError('accepted and durable run identities do not match exactly')
    if any(row['status'] != 'completed' for row in rows):
        raise RuntimeError('experiment includes unsuccessful or unfinished runs')
    return {'completed': len(rows)}


def summarize_runs(rows):
    buckets, latency = {}, []
    for row in rows:
        if row['status'] != 'completed':
            continue
        end = datetime.datetime.fromisoformat(row['completedAt']).astimezone(datetime.timezone.utc)
        start = datetime.datetime.fromisoformat(row['createdAt'])
        key = end.isoformat()[:16]
        buckets[key] = buckets.get(key, 0) + 1
        latency.append((end - start).total_seconds() * 1000)
    latency.sort()
    return {'completed': len(latency), 'failed': sum(r['status'] == 'failed' for r in rows),
            'statuses': {status: sum(r['status'] == status for r in rows) for status in sorted({r['status'] for r in rows})},
            'completionBuckets': dict(sorted(buckets.items())),
            'completedLatencyMs': {key: latency[int((len(latency) - 1) * p)] if latency else None
                                   for key, p in [('p50', .5), ('p95', .95), ('p99', .99)]}}


def failure_record(phase, error, secrets=()):
    message = str(error)
    for secret in secrets:
        if secret:
            message = message.replace(secret, '[redacted]')
    return {'phase': phase, 'errorType': type(error).__name__, 'message': message[:1000]}


def load(stack, output, rate, seconds):
    if not 1 <= rate <= 600 or not 1 <= seconds <= 3600:
        raise ValueError('load requires rate 1-600/min and duration 1-3600 seconds')
    if not stack.quiet():
        raise RuntimeError('previous work has not drained')
    binary = ROOT / 'perf/k6/bin' / ('k6.exe' if os.name == 'nt' else 'k6')
    (output / 'k6-version.txt').write_text(run([str(binary), 'version']), encoding='utf-8')
    (output / 'baseline.json').write_text(stack.sql(
        "SELECT json_build_object('agentRuns',(SELECT count(*) FROM app_core_agentrun),"
        "'events',(SELECT count(*) FROM app_core_sessionevent))"), encoding='utf-8')
    env = {**compose_command(ROOT, stack.env_file, [])[1], 'API_BASE': 'http://localhost:18000',
           'PERF_ADMIN_EMAIL': stack.values['BOOTSTRAP_SUPERADMIN_EMAIL'],
           'PERF_ADMIN_PASSWORD': stack.values['BOOTSTRAP_SUPERADMIN_PASSWORD'],
           'RATE': str(rate), 'DURATION': f'{seconds}s', 'VERBOSE': '1'}
    command = [str(binary), 'run', '--summary-export', str(output / 'k6-summary.json'),
               str(ROOT / 'perf/k6/scenarios/s2-runs.js')]
    failures = []
    private = [value for key, value in stack.values.items() if any(word in key for word in ("PASSWORD", "SECRET", "TOKEN", "KEY"))]
    started = time.monotonic()
    workload_elapsed = None
    with (output / 'k6.log').open('w', encoding='utf-8') as log, (output / 'resources.jsonl').open('x', encoding='utf-8') as samples:
        process = subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT)
        try:
            deadline = started + seconds + 360
            while True:
                try:
                    sample = {'utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
                              'resources': stack.sample(),
                              'runCounts': stack.sql('SELECT status,count(*) FROM app_core_agentrun GROUP BY status').splitlines()}
                    samples.write(json.dumps(sample) + '\n')
                    samples.flush()
                except Exception as error:
                    failures.append(failure_record('sampling', error, private))
                if process.poll() is not None:
                    if workload_elapsed is None:
                        workload_elapsed = time.monotonic() - started
                        if process.returncode:
                            failures.append(failure_record('k6', RuntimeError(f'k6 exit {process.returncode}')))
                    try:
                        if stack.quiet():
                            break
                    except Exception as error:
                        failures.append(failure_record('drain-check', error, private))
                if time.monotonic() >= deadline:
                    failures.append(failure_record('deadline', TimeoutError('experiment did not drain before deadline')))
                    break
                time.sleep(2)
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                failures.append(failure_record('k6', RuntimeError('k6 terminated before natural exit')))
    accepted = re.findall(r'accepted run ([A-Za-z0-9_-]+) session ', (output / 'k6.log').read_text(encoding='utf-8'))
    (output / 'accepted-ids.json').write_text(json.dumps(accepted), encoding='utf-8')
    rows = []
    try:
        if not accepted:
            raise RuntimeError('no accepted run identities captured')
        ids = ','.join("'" + value + "'" for value in accepted)
        rows = json.loads(stack.sql("SELECT coalesce(json_agg(t),'[]'::json) FROM "
            '(SELECT id,status,"createdAt","startedAt","completedAt","transitionReason" '
            f'FROM app_core_agentrun WHERE id IN ({ids}) ORDER BY "createdAt") t'))
        (output / 'runs.json').write_text(json.dumps(rows, indent=2), encoding='utf-8')
        k6_summary = json.loads((output / 'k6-summary.json').read_text(encoding='utf-8'))
        account_runs(accepted, rows, k6_summary['metrics']['s2_runs_accepted']['count'])
    except Exception as error:
        failures.append(failure_record('accounting', error, private))
    summary = summarize_runs(rows)
    summary.update(rate=rate, requestedDurationSeconds=seconds, observedWorkloadSeconds=workload_elapsed,
                   k6ExitCode=process.returncode, valid=not failures, acceptedCount=len(accepted),
                   durableCount=len(rows), historyMode='continuous-history')
    (output / 'outcome.json').write_text(json.dumps(summary, indent=2), encoding='utf-8')
    if failures:
        (output / 'failure.json').write_text(json.dumps({'valid': False, 'errors': failures}, indent=2), encoding='utf-8')
        raise RuntimeError('experiment failed; see outcome.json and failure.json')


class Stack:
    def __init__(self):
        self.env_file = ROOT / 'perf/.state/test.env'
        self.values = read_env(self.env_file)
        if self.values.get('COMPOSE_PROJECT_NAME') != PROJECT:
            raise ValueError('test env must identify the performance project')
        validate_config(json.loads(self.compose(['config', '--format', 'json'])), ROOT)

    def compose(self, args, slots=None, timeout=120, **kwargs):
        command, env = compose_command(ROOT, self.env_file, args, slots)
        return run(command, env=env, cwd=ROOT, timeout=timeout, **kwargs)

    def container(self, service):
        ids = self.compose(['ps', '-q', service]).split()
        if len(ids) != 1:
            raise RuntimeError(f'expected one running {service} container')
        container = json.loads(run(['docker', 'inspect', ids[0]]))[0]
        if container['Config']['Labels'].get('com.docker.compose.project') != PROJECT:
            raise ValueError('foreign container')
        return container

    def identities(self):
        ids = self.compose(['ps', '--all', '-q']).split()
        containers = json.loads(run(['docker', 'inspect', *ids])) if ids else []
        return {c['Name']: c['Id'] for c in containers
                if c['Config']['Labels'].get('com.docker.compose.service') != 'worker'}

    def sql(self, statement):
        return self.compose(['exec', '-T', 'postgres', 'psql', '-v', 'ON_ERROR_STOP=1',
                             '-U', self.values['POSTGRES_USER'], '-d', self.values['POSTGRES_DB'], '-At'], input=statement)

    def quiet(self):
        return self.sql("SELECT count(*) FROM app_core_agentrun WHERE status IN ('queued','running')").strip() == '0'

    def verify_slots(self, expected):
        result = self.compose(['exec', '-T', 'worker', 'python', '-c', 'import worker; print(worker.WORKER_SLOT_COUNT)'])
        if result.strip() != str(expected):
            raise RuntimeError('worker slot setting did not take effect')

    def preflight(self):
        for service in SERVICES:
            container = self.container(service)
            if container['State'].get('Health', {}).get('Status', 'healthy') != 'healthy':
                raise RuntimeError(f'{service} is not healthy')
        self.compose(['exec', '-T', 'api', 'python', '-c',
                      "import socket,ssl; s=socket.create_connection(('mock-model',9999),timeout=5); "
                      "ssl.create_default_context().wrap_socket(s,server_hostname='mock-model').close()"])
        self.sample()

    def sample(self):
        containers = [self.container(service) for service in SERVICES]
        ids = [c['Id'] for c in containers]
        expected = {c['Name'].lstrip('/') for c in containers}
        return parse_stats(run(['docker', 'stats', '--no-stream', '--format', '{{json .}}', *ids]), expected)

    def manifest(self):
        revision = run(['git', 'rev-parse', 'HEAD'], cwd=ROOT).strip()
        return {'project': PROJECT, 'workspaceSha': revision,
                'workerSlots': int(self.compose(['exec', '-T', 'worker', 'python', '-c', 'import worker; print(worker.WORKER_SLOT_COUNT)']).strip()),
                'coreSha': revision,
                'dirtyFiles': run(['git', 'status', '--porcelain'], cwd=ROOT).splitlines(),
                'workspaceDiffSha256': hashlib.sha256(run(['git', 'diff', 'HEAD'], cwd=ROOT).encode()).hexdigest(),
                'workerSourceSha256': hashlib.sha256((ROOT / 'packages/worker/worker.py').read_bytes()).hexdigest(),
                'containers': {s: {'id': (c := self.container(s))['Id'], 'image': c['Image'],
                                   'startedAt': c['State']['StartedAt']} for s in SERVICES}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['init', 'check', 'build', 'up', 'slots', 'smoke', 'sample', 'load', 'stop'])
    parser.add_argument('--slots', type=int)
    parser.add_argument('--experiment-id')
    parser.add_argument('--seconds', type=int, default=1)
    parser.add_argument('--rate', type=int, default=60)
    parser.add_argument('--oci-runtime', choices=['runsc', 'runc'],
                        help='Explicit sandbox runtime for init; runc supports hosts without runsc.')
    args = parser.parse_args()
    if args.oci_runtime is not None and args.action != 'init':
        parser.error('--oci-runtime only applies to init; it does not change an existing stack')
    if args.action == 'init':
        initialize(ROOT, oci_runtime=args.oci_runtime)
        print('Created perf/.state/test.env; root .env was not read or changed.')
        return
    stack = Stack()
    if args.action == 'check':
        print('Isolated Compose configuration verified.')
    elif args.action == 'build':
        stack.compose(['build', 'api', 'worker', 'runtime', 'workspace-general', 'mock-model'], timeout=3600)
    elif args.action == 'up':
        stack.compose(['up', '-d', 'worker'], timeout=600)
        stack.sql('CREATE EXTENSION IF NOT EXISTS pg_stat_statements;')
        stack.preflight()
    elif args.action == 'slots':
        switch_slots(stack, args.slots)
        stack.preflight()
    elif args.action == 'stop':
        if not stack.quiet():
            raise RuntimeError('stack must be idle before stopping')
        stack.compose(['stop'])
    elif args.action in ('smoke', 'sample', 'load'):
        if not args.experiment_id or not all(c.isalnum() or c in '-_' for c in args.experiment_id):
            raise ValueError('a unique alphanumeric experiment-id is required')
        output = ROOT.parent / 'centaeris-perf-evidence' / args.experiment_id
        output.mkdir(parents=True, exist_ok=False)
        stack.preflight()
        (output / 'manifest.json').write_text(json.dumps(stack.manifest(), indent=2), encoding='utf-8')
        (output / 'sample.json').write_text(json.dumps(stack.sample(), indent=2), encoding='utf-8')
        if args.action == 'load':
            load(stack, output, args.rate, args.seconds)
        elif args.action == 'smoke':
            env = {**os.environ, 'API_BASE': 'http://localhost:18000',
                   'PERF_ADMIN_EMAIL': stack.values['BOOTSTRAP_SUPERADMIN_EMAIL'],
                   'PERF_ADMIN_PASSWORD': stack.values['BOOTSTRAP_SUPERADMIN_PASSWORD']}
            result = run([sys.executable, str(ROOT / 'perf/harness/bootstrap.py')], env=env, timeout=190)
            (output / 'smoke.txt').write_text(result, encoding='utf-8')
        else:
            if not 1 <= args.seconds <= 14400:
                raise ValueError('sample duration must be between 1 and 14400 seconds')
            deadline = time.monotonic() + args.seconds
            with (output / 'resources.jsonl').open('x', encoding='utf-8') as handle:
                while time.monotonic() < deadline:
                    row = {'utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
                           'resources': stack.sample(),
                           'runCounts': stack.sql('SELECT status,count(*) FROM app_core_agentrun GROUP BY status').splitlines()}
                    handle.write(json.dumps(row) + '\n')
                    handle.flush()
                    time.sleep(2)
        print(f'Evidence: {output}')


if __name__ == '__main__':
    main()
