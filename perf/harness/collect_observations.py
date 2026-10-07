"""Read-only observation collection for an existing isolated experiment."""
import argparse
import datetime
import json
import re
import subprocess
import time

if __package__:
    from .control import ROOT, Stack
else:
    from control import ROOT, Stack

DB_SAMPLE = """SELECT json_build_object(
 'activity',(SELECT COALESCE(json_agg(s),'[]') FROM (
   SELECT state,wait_event_type,wait_event,count(*) FROM pg_stat_activity
   WHERE datname=current_database() AND pid<>pg_backend_pid()
   GROUP BY state,wait_event_type,wait_event) s),
 'locks',(SELECT COALESCE(json_agg(s),'[]') FROM (
   SELECT locktype,mode,granted,count(*) FROM pg_locks
   WHERE pid IN (SELECT pid FROM pg_stat_activity WHERE datname=current_database())
   GROUP BY locktype,mode,granted) s))"""


def records(text):
    for line in text.splitlines():
        try:
            value = json.loads(line)
        except ValueError:
            continue
        if isinstance(value, dict) and value.get('schema') == 'workspace.perf.v1':
            yield value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--experiment-id', required=True)
    parser.add_argument('--sample-db', type=int, metavar='SECONDS')
    parser.add_argument('--since')
    parser.add_argument('--until')
    args = parser.parse_args()
    if not re.fullmatch(r'[A-Za-z0-9_-]{1,96}', args.experiment_id):
        parser.error('invalid experiment id')
    output = ROOT.parent / 'centaeris-perf-evidence' / args.experiment_id
    if not (output / 'manifest.json').is_file():
        parser.error('experiment manifest must already exist')
    if args.sample_db is not None:
        if not 1 <= args.sample_db <= 900:
            parser.error('sampling must last 1..900 seconds')
        stack = Stack()
        end = time.monotonic() + args.sample_db
        with (output / 'db-waits.jsonl').open('x', encoding='utf-8') as handle:
            while time.monotonic() < end:
                sample = {'utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
                          'snapshot': json.loads(stack.sql(DB_SAMPLE))}
                handle.write(json.dumps(sample) + '\n')
                handle.flush()
                time.sleep(min(10, max(0, end - time.monotonic())))
        return
    if not args.since or not args.until:
        parser.error('log collection requires --since and --until UTC timestamps')
    manifest = json.loads((output / 'manifest.json').read_text(encoding='utf-8'))
    for service in ('worker', 'runtime'):
        identity = manifest['containers'][service]['id']
        if not re.fullmatch(r'[0-9a-f]{64}', identity):
            parser.error('invalid container identity')
        result = subprocess.run(['docker', 'logs', '--since', args.since, '--until', args.until, identity],
                                capture_output=True, encoding='utf-8', check=True, timeout=60)
        with (output / f'{service}-observations.jsonl').open('x', encoding='utf-8') as handle:
            count = 0
            for record in records(result.stdout + '\n' + result.stderr):
                handle.write(json.dumps(record) + '\n')
                count += 1
        if not count:
            raise SystemExit(f'{service}: no observations; do not treat the experiment as instrumented')


if __name__ == '__main__':
    main()
