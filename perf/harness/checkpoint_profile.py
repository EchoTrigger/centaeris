"""Profile checkpoint scans in a fresh database on an explicit test PostgreSQL.

The caller supplies a PostgreSQL instance with pg_stat_statements preloaded. This
harness creates and drops only its UUID-named database; it never resets a default
database or controls a deployment/perf stack.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time
import uuid
from urllib.parse import quote

MARKER = 'CHECKPOINT_PROFILE_JSON='


def settings():
    return {key: os.environ.get('TEST_POSTGRES_' + env, default) for key, env, default in (
        ('host', 'HOST', 'localhost'), ('port', 'PORT', '55432'),
        ('dbname', 'DB', 'centaeris'), ('user', 'USER', 'centaeris'),
        ('password', 'PASSWORD', 'centaeris'))}


def result_from_output(output):
    lines = [line.split(MARKER, 1)[1] for line in output.splitlines() if MARKER in line]
    if len(lines) != 1:
        raise RuntimeError(f'expected one checkpoint profile result, found {len(lines)}')
    return json.loads(lines[0])


def database_url(config, database):
    host = config['host']
    if ':' in host and not host.startswith('['):
        host = '[' + host + ']'
    return (f"postgresql://{quote(config['user'], safe='')}:{quote(config['password'], safe='')}"
            f"@{host}:{config['port']}/{database}")


def run_case(root, env, waiting, source, output_dir, timeout_seconds):
    case_env = {**env, 'CENTAERIS_CHECKPOINT_PROFILE_WAITING': str(waiting),
                'CENTAERIS_CHECKPOINT_PROFILE_SOURCE': source}
    started = time.perf_counter()
    completed = subprocess.run(
        ['cargo', 'test', '--locked', '-p', 'runtime_server',
         'postgres_checkpoint_profile_reconcile_waiters', '--', '--ignored', '--nocapture',
         '--test-threads=1'], cwd=root, env=case_env, capture_output=True, text=True,
        timeout=timeout_seconds)
    stem = output_dir / f'waiting-{waiting}-{source}'
    stem.with_suffix('.stdout.txt').write_text(completed.stdout, encoding='utf-8')
    stem.with_suffix('.stderr.txt').write_text(completed.stderr, encoding='utf-8')
    if completed.returncode:
        raise RuntimeError(completed.stdout + completed.stderr)
    result = result_from_output(completed.stdout)
    result['processElapsedMs'] = (time.perf_counter() - started) * 1000
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--stop-ms', type=float, default=30_000)
    parser.add_argument('--case-timeout-seconds', type=float, default=300)
    parser.add_argument('--waiting', type=int, action='append', choices=(0, 1, 256, 257, 1024))
    parser.add_argument('--source', action='append', choices=('nonterminal', 'terminal'))
    args = parser.parse_args()
    if args.output.exists():
        raise FileExistsError(args.output)
    import psycopg
    from psycopg import sql

    root = Path(__file__).resolve().parents[2]
    config = settings()
    database = 'test_checkpoint_profile_' + uuid.uuid4().hex
    url = database_url(config, database)
    env = {**os.environ, 'CENTAERIS_ALLOW_POSTGRES_TEST_RESET': '1',
           'CENTAERIS_TEST_POSTGRES_URL': url, 'INTERNAL_API_TOKEN': uuid.uuid4().hex,
           'CARGO_BUILD_JOBS': '1'}
    results = []
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with psycopg.connect(**config, autocommit=True) as admin:
        admin.execute(sql.SQL('CREATE DATABASE {}').format(sql.Identifier(database)))
        try:
            with psycopg.connect(url, autocommit=True) as profile:
                preload = profile.execute('SHOW shared_preload_libraries').fetchone()[0]
                if 'pg_stat_statements' not in preload.split(','):
                    raise RuntimeError('profile PostgreSQL must preload pg_stat_statements')
                profile.execute('CREATE EXTENSION pg_stat_statements')
            stopped = False
            sizes = args.waiting or [0, 1, 257, 1024]
            sources = args.source or ['nonterminal', 'terminal']
            for waiting in sizes:
                if stopped:
                    break
                for source in sources:
                    result = run_case(root, env, waiting, source, args.output.parent,
                                      args.case_timeout_seconds)
                    results.append(result)
                    args.output.write_text(json.dumps(results, indent=2), encoding='utf-8')
                    if waiting >= 257 and any(
                            item['elapsedMs'] > args.stop_ms for item in result['measurements']):
                        stopped = True
                        result['expansionStoppedAfterCompletedCase'] = {
                            'thresholdMs': args.stop_ms,
                            'meaning': 'the completed request was retained; only larger sizes are skipped',
                        }
                        args.output.write_text(json.dumps(results, indent=2), encoding='utf-8')
                        break
        finally:
            admin.execute(sql.SQL('DROP DATABASE {} WITH (FORCE)').format(sql.Identifier(database)))
    print(args.output)


if __name__ == '__main__':
    main()
