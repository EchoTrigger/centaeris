"""Compare history amplification through real HTTP endpoints in the isolated stack.

Only synthetic perf.outbox.history fixtures are replaced. The worker is paused
while idle, then restarted; API/runtime and all non-test projects are untouched.
This is a bounded replay-work probe, not a capacity or CPU steady-state claim.
"""
import argparse
import json
import time

from control import ROOT, Stack

PREFIX = 'perf.outbox.history:'


def seed(stack, size):
    if type(size) is not int or not 0 <= size <= 100_000:
        raise ValueError('invalid fixture size')
    stack.sql(f"""
        BEGIN;
        DELETE FROM runtime.runtime_jobs
        WHERE job_kind='perf.outbox.history' AND job_id LIKE '{PREFIX}%';
        INSERT INTO runtime.runtime_jobs
        (job_id,job_kind,status,run_at_ms,backoff_policy_json,idempotency_key,created_at_ms,updated_at_ms)
        SELECT '{PREFIX}' || n, 'perf.outbox.history', 'succeeded', 1,
          '{{"baseDelayMs":2000,"maxDelayMs":60000,"multiplier":2.0,"jitterMs":250}}',
          '{PREFIX}' || n, 1, 20 FROM generate_series(1,{size}) n;
        INSERT INTO runtime.runtime_job_outbox(job_id,event_type,published_at_ms,generation)
        SELECT job_id,'runtime_job.terminal',1,0 FROM runtime.runtime_jobs
        WHERE job_kind='perf.outbox.history' AND job_id LIKE '{PREFIX}%';
        COMMIT;
        ANALYZE runtime.runtime_jobs;
        ANALYZE runtime.runtime_job_outbox;
    """)


def requests(stack, now):
    # Token stays inside the API container and never enters arguments or receipts.
    program = f"""
import json,os,time,urllib.request
results=[]
for path,body in [
('/internal/jobs/reconcile',{{'schema':'runtime.job.reconcile.v1','nowMs':{now}}}),
('/internal/job-outbox/pending',{{'schema':'runtime.job.outbox.pending.v1','limit':256}}),
('/internal/job-outbox/reconcile-waiters',{{'schema':'runtime.job.waiters.reconcile.v1'}})]:
    request=urllib.request.Request('http://runtime:{stack.values['RUNTIME_PORT']}'+path,
        data=json.dumps(body).encode(),headers={{'Content-Type':'application/json','X-Internal-Token':os.environ['INTERNAL_API_TOKEN']}})
    start=time.perf_counter()
    with urllib.request.urlopen(request,timeout=30) as response:
        value=json.load(response)
    if 'events' in value: value={{'pendingPageSize':len(value['events'])}}
    results.append({{'path':path,'elapsedMs':(time.perf_counter()-start)*1000,'response':value}})
print(json.dumps(results))
"""
    return json.loads(stack.compose(['exec', '-T', 'api', 'python', '-c', program]))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--experiment-id', required=True)
    args = parser.parse_args()
    if not all(c.isalnum() or c in '-_' for c in args.experiment_id):
        raise ValueError('invalid experiment ID')
    output = ROOT.parent / 'centaeris-perf-evidence' / args.experiment_id
    output.mkdir(parents=True, exist_ok=False)
    stack = Stack()
    stack.preflight()
    if not stack.quiet():
        raise RuntimeError('active work must drain first')
    if stack.sql("SELECT count(*) FROM runtime.checkpoints WHERE kind='wait' AND status='waiting' AND done_reason='runtime_job'").strip() != '0':
        raise RuntimeError('history-only probe requires zero actual waiters')
    (output / 'manifest.json').write_text(json.dumps(stack.manifest(), indent=2), encoding='utf-8')
    stack.sql('CREATE EXTENSION IF NOT EXISTS pg_stat_statements')
    stack.compose(['stop', 'worker'])
    result = []
    try:
        for size in (0, 3641, 36410):
            seed(stack, size)
            stage = {'historyRows': size, 'rounds': []}
            for iteration in range(3):
                # Simulate completed delivery of fixture notifications, keeping them old.
                stack.sql(f"UPDATE runtime.runtime_job_outbox SET published_at_ms=1 WHERE job_id LIKE '{PREFIX}%'")
                stack.sql('SELECT pg_stat_statements_reset()')
                response = requests(stack, int(time.time() * 1000) + iteration * 60_000)
                stats = json.loads(stack.sql("""SELECT coalesce(json_agg(t),'[]'::json) FROM (
                    SELECT query,calls,rows,total_exec_time,shared_blks_hit,shared_blks_read
                    FROM pg_stat_statements WHERE query ILIKE '%runtime_job_outbox%'
                    AND query NOT ILIKE '%pg_stat_statements%'
                ) t"""))
                counts = json.loads(stack.sql(f"""SELECT json_build_object(
                    'pending',count(*) FILTER(WHERE published_at_ms IS NULL),
                    'generationSum',coalesce(sum(generation),0))
                    FROM runtime.runtime_job_outbox WHERE job_id LIKE '{PREFIX}%'"""))
                stage['rounds'].append({'http': response, 'fixture': counts, 'sql': stats})
            result.append(stage)
            (output / 'results.json').write_text(json.dumps(result, indent=2), encoding='utf-8')
    finally:
        # Keep the largest acknowledged fixture as evidence, without a replay backlog.
        stack.sql(f"UPDATE runtime.runtime_job_outbox SET published_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE job_id LIKE '{PREFIX}%'")
        stack.compose(['start', 'worker'])
    print(output)


if __name__ == '__main__':
    main()
