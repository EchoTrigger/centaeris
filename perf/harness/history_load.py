"""Bounded load comparison at 0/3641/36410/0 acknowledged fixture rows."""
import argparse
import json
import time

from control import ROOT, Stack, load
from outbox_profile import seed


def database_delta(before, after):
    seconds = after['epoch'] - before['epoch']
    sessions = after['sessions'] - before['sessions']
    if seconds <= 0 or sessions < 0 or before['statsReset'] != after['statsReset']:
        raise ValueError('invalid database measurement window')
    return dict(seconds=seconds, sessions=sessions, connectionsPerSecond=sessions / seconds)


def snapshot(stack):
    return json.loads(stack.sql("""SELECT json_build_object(
        'epoch',EXTRACT(EPOCH FROM clock_timestamp()),
        'sessions',sessions,'statsReset',stats_reset,
        'agentRuns',(SELECT count(*) FROM app_core_agentrun),
        'events',(SELECT count(*) FROM app_core_sessionevent),
        'outbox',(SELECT count(*) FROM runtime.runtime_job_outbox),
        'fixturePending',(SELECT count(*) FROM runtime.runtime_job_outbox
            WHERE job_id LIKE 'perf.outbox.history:%' AND published_at_ms IS NULL),
        'fixtureGeneration',(SELECT coalesce(sum(generation),0) FROM runtime.runtime_job_outbox
            WHERE job_id LIKE 'perf.outbox.history:%'),
        'statements',(SELECT coalesce(json_agg(s),'[]'::json) FROM (
            SELECT userid,dbid,toplevel,queryid,query,calls,rows,total_exec_time,
                shared_blks_hit,shared_blks_read FROM pg_stat_statements
            WHERE dbid=(SELECT oid FROM pg_database WHERE datname=current_database())
        ) s)) FROM pg_stat_database WHERE datname=current_database()"""))


def write(path, value):
    path.write_text(json.dumps(value, indent=2), encoding='utf-8')


def prepare(stack, size):
    if not stack.quiet():
        raise RuntimeError('active runs must drain before fixture replacement')
    if stack.sql("SELECT count(*) FROM runtime.checkpoints WHERE kind='wait' AND status='waiting' AND done_reason='runtime_job'").strip() != '0':
        raise RuntimeError('active waiters must drain before fixture replacement')
    identities = stack.identities()
    stack.compose(['stop', 'worker'])
    try:
        seed(stack, size)
    finally:
        stack.compose(['start', 'worker'])
    if stack.identities() != identities:
        raise RuntimeError('non-worker container identity changed')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--experiment-id', required=True)
    args = parser.parse_args()
    if not args.experiment_id or not all(c.isalnum() or c in '-_' for c in args.experiment_id):
        raise ValueError('invalid experiment ID')
    output = ROOT.parent / 'centaeris-perf-evidence' / args.experiment_id
    output.mkdir(parents=True, exist_ok=False)
    stack = Stack()
    stack.preflight()
    stack.verify_slots(2)
    write(output / 'manifest.json', stack.manifest())
    write(output / 'design.json', dict(rate=30, seconds=90, slots=2,
          fixtureRows=[0, 3641, 36410, 0],
          historyMode='controlled synthetic outbox; continuous real run history',
          window='database snapshots bracket workload setup, injection and drain'))
    outcomes = []
    for index, size in enumerate((0, 3641, 36410, 0)):
        stage = output / f'{index}-history-{size}'
        stage.mkdir()
        prepare(stack, size)
        time.sleep(10)
        before = snapshot(stack)
        write(stage / 'database-t0.json', before)
        print(f'Start stage {index}: history={size}, rate=30/min, seconds=90', flush=True)
        try:
            load(stack, stage, 30, 90)
        finally:
            after = snapshot(stack)
            write(stage / 'database-t1.json', after)
        if after['fixturePending'] or after['fixtureGeneration']:
            raise RuntimeError('acknowledged fixture history was reactivated')
        outcome = dict(historyRows=size, **database_delta(before, after))
        outcomes.append(outcome)
        write(output / 'windows.json', outcomes)
        print(f'Completed stage {index}: {outcome}', flush=True)
    # Retain the established large-history baseline after successful draining.
    prepare(stack, 36410)
    write(output / 'final.json', snapshot(stack))
    print(f'Evidence: {output}', flush=True)


if __name__ == '__main__':
    main()
