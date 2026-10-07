"""Bounded connection-pool measurement; only the owned centaeris-perf stack may be changed."""
import argparse
import datetime
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time

import control

ROOT = control.ROOT
PROBE_PREFIX = 'CONNECTION_POOL_PROBE '


def write(path, value):
    path.write_text(json.dumps(value, indent=2), encoding='utf-8')


def pool_compose_command(env_file, args):
    command, env = control.compose_command(ROOT, env_file, [])
    return [*command, '-f', str(ROOT / 'perf/compose.pool.yml'), *args], env


class PoolStack(control.Stack):
    def compose(self, args, slots=None, timeout=120, **kwargs):
        if slots is not None:
            raise ValueError('Pool validation fixes worker slots to two')
        command, env = pool_compose_command(self.env_file, args)
        return control.run(command, env=env, cwd=ROOT, timeout=timeout, **kwargs)

    def all_identities(self):
        ids = self.compose(['ps', '--all', '-q']).split()
        rows = json.loads(control.run(['docker', 'inspect', *ids])) if ids else []
        if any(row['Config']['Labels'].get('com.docker.compose.project') != control.PROJECT for row in rows):
            raise RuntimeError('foreign container in pool validation stack')
        return {row['Name']: row['Id'] for row in rows}


def verify_api_replacement(before, after):
    is_api = lambda name: name == 'api' or name.endswith('-api-1')
    old_api = {key: value for key, value in before.items() if is_api(key)}
    new_api = {key: value for key, value in after.items() if is_api(key)}
    if len(old_api) != 1 or old_api.keys() != new_api.keys() or old_api == new_api:
        raise RuntimeError('API was not replaced exactly once')
    if {k: v for k, v in before.items() if not is_api(k)} != {k: v for k, v in after.items() if not is_api(k)}:
        raise RuntimeError('API replacement changed another service')


def check_database_budget(sample):
    maximum, count = sample.get('maxConnections'), sample.get('connections')
    if type(maximum) is not int or type(count) is not int or maximum <= 0 or count < 0:
        raise RuntimeError('missing database budget sample')
    if count >= maximum * .8:
        raise RuntimeError('database connections reached 80% stop threshold')


def validate_probe(rows, size):
    samples = [row for row in rows if row.get('kind') == 'sample']
    if not samples or any(row.get('kind') in {'httpError', 'exception', 'sampleError'} for row in rows):
        raise RuntimeError('missing pool samples or an API error occurred')
    states = {row['pool']['state'] for row in samples}
    if size == 0:
        if states != {'disabled'}:
            raise RuntimeError('rollback did not disable the serving pool')
        return
    active = [row['pool']['stats'] for row in samples if row['pool']['state'] == 'active']
    if not active:
        raise RuntimeError('no active serving pool was observed')
    for stats in active:
        if type(stats.get('pool_size')) is not int or stats['pool_size'] > size:
            raise RuntimeError('invalid or oversized serving pool')
        if stats.get('requests_errors', 0) or stats.get('connections_errors', 0):
            raise RuntimeError('connection pool reported request/connection errors')


def set_pool(size):
    path = ROOT / 'perf/.state/test.env'
    rows = path.read_text(encoding='utf-8').splitlines()
    rows = [row for row in rows if not row.startswith('API_POSTGRES_POOL_MAX_SIZE=')]
    path.write_text('\n'.join([*rows, f'API_POSTGRES_POOL_MAX_SIZE={size}']) + '\n', encoding='utf-8')


def wait_healthy(stack):
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        if stack.container('api')['State'].get('Health', {}).get('Status') == 'healthy':
            return
        time.sleep(1)
    raise RuntimeError('API health deadline exceeded')


def database_sample(stack):
    return json.loads(stack.sql("SELECT json_build_object('maxConnections',current_setting('max_connections')::int,"
        "'connections',(SELECT count(*) FROM pg_stat_activity WHERE backend_type='client backend'),"
        "'byApplication',(SELECT json_agg(t) FROM (SELECT application_name,count(*) AS connections,"
        "count(*) FILTER (WHERE state='active') AS active FROM pg_stat_activity "
        "WHERE backend_type='client backend' GROUP BY application_name) t))"))


def stage(stack, output, size):
    output.mkdir()
    if not stack.quiet():
        raise RuntimeError('Pool validation requires an idle isolated stack')
    before = stack.all_identities()
    set_pool(size)
    stack.compose(['up', '-d', '--no-deps', '--force-recreate', 'api'], timeout=180)
    wait_healthy(stack)
    after = stack.all_identities()
    verify_api_replacement(before, after)
    write(output / 'replacement.json', {'before': before, 'after': after})
    stack.preflight()
    stack.verify_slots(2)
    write(output / 'manifest.json', stack.manifest())
    start = datetime.datetime.now(datetime.timezone.utc).isoformat()
    process = None
    failure = None
    try:
        with (output / 'workload.log').open('w', encoding='utf-8') as log:
            process = subprocess.Popen([sys.executable, __file__, 'child', '--output', str(output)],
                cwd=ROOT, stdout=log, stderr=subprocess.STDOUT,
                env={**os.environ, 'NO_PROXY': '127.0.0.1,localhost,::1'})
            deadline = time.monotonic() + 210
            with (output / 'samples.jsonl').open('w', encoding='utf-8') as samples:
                while process.poll() is None:
                    if time.monotonic() >= deadline:
                        raise RuntimeError('Pool validation workload exceeded 210 second hard deadline')
                    sample = {'utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
                              'database': database_sample(stack), 'resources': stack.sample()}
                    samples.write(json.dumps(sample) + '\n')
                    samples.flush()
                    check_database_budget(sample['database'])
                    for service in control.SERVICES:
                        state = stack.container(service)['State']
                        if state.get('OOMKilled') or state.get('Status') != 'running':
                            raise RuntimeError('service exited or exceeded memory budget')
                    time.sleep(2)
            if process.returncode:
                raise RuntimeError('Pool validation workload failed; see workload evidence')
    except Exception as error:
        failure = error
        (output / 'stop').touch()
    finally:
        if process is not None and process.poll() is None:
            try:
                process.wait(timeout=35)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=10)
        for service in ('api', 'worker', 'runtime', 'postgres'):
            logs = stack.compose(['logs', '--no-color', '--since', start, service])
            for secret in stack.values.values():
                if len(secret) >= 16:
                    logs = logs.replace(secret, '[redacted]')
            (output / f'{service}.log').write_text(logs, encoding='utf-8')
        rows = []
        for line in (output / 'api.log').read_text(encoding='utf-8').splitlines():
            if PROBE_PREFIX in line:
                rows.append(json.loads(line.split(PROBE_PREFIX, 1)[1]))
        write(output / 'pool-samples.json', rows)
    if failure:
        raise failure
    validate_probe(rows, size)
    if not stack.quiet():
        raise RuntimeError('workload left active Runs')
    write(output / 'verified.json', {'valid': True, 'poolMaxSize': size})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['build', 'up', 'run', 'child', 'stop'])
    parser.add_argument('--experiment-id')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    stack = PoolStack()
    if args.action == 'build':
        stack.compose(['build', 'api'], timeout=300)
    elif args.action == 'up':
        stack.compose(['up', '-d', 'worker'], timeout=600)
        stack.sql('CREATE EXTENSION IF NOT EXISTS pg_stat_statements')
        stack.preflight()
    elif args.action == 'stop':
        if not stack.quiet():
            raise RuntimeError('active Runs require investigation before stopping')
        stack.compose(['stop'])
    elif args.action == 'child':
        from pool_workload import run_workload
        stop = threading.Event()
        def watch():
            while not stop.wait(.5):
                if (args.output / 'stop').exists():
                    stop.set()
        threading.Thread(target=watch, daemon=True).start()
        try:
            result = run_workload(stack, args.output, seconds=60, interval=5, observers=3, stop=stop)
            if not result['ok']:
                raise RuntimeError('bounded workload did not pass; see workload.json')
        finally:
            stop.set()
    else:
        name = args.experiment_id
        if not name or not all(c.isalnum() or c in '-_' for c in name):
            raise ValueError('unique alphanumeric experiment ID required')
        output = ROOT.parent / 'centaeris-perf-evidence' / name
        output.mkdir(exist_ok=False)
        write(output / 'design.json', {'stages': [8, 0], 'apiWorkers': 1, 'workerSlots': 2,
            'arrivalSeconds': 60, 'arrivalIntervalSeconds': 5, 'observersPerRun': 3,
            'maximumOutstandingRuns': 2, 'hardDeadlineSeconds': 210,
            'connectionStopFraction': .8, 'historyMode': 'continuous-history',
            'stopOn': ['request error', 'Run failure', 'SSE failure', 'service exit/OOM', 'connection budget'],
            'purpose': 'bounded correctness and API-only rollback; not throughput certification'})
        original = stack.env_file.read_bytes()
        try:
            for size in (8, 0):
                print(f'Pool validation stage pool={size}', flush=True)
                stage(stack, output / f'pool-{size}', size)
            write(output / 'result.json', {'valid': True, 'stages': [8, 0]})
        except Exception as error:
            write(output / 'failure.json', {'valid': False, 'errorType': type(error).__name__, 'message': str(error)})
            raise
        finally:
            stack.env_file.write_bytes(original)


if __name__ == '__main__':
    main()
