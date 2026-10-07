"""Measure the SQL scan used to find an old recovery checkpoint.

This is a SQL-only probe over the real runtime.checkpoints table. It does not
exercise AgentRun restart validation, RuntimeStore connection creation, or HTTP.
"""
import argparse
import json
import os
from pathlib import Path
import time

QUERY = """SELECT checkpoint_id,kind,session_id,turn_id,status,done_reason,
updated_at_ms,payload_json FROM runtime.checkpoints WHERE session_id=%s
ORDER BY updated_at_ms DESC,checkpoint_id DESC LIMIT %s OFFSET %s"""


def statement_snapshot(connection):
    rows = connection.execute("""SELECT queryid::text,query,calls::bigint,rows::bigint,
total_exec_time,shared_blks_hit,shared_blks_read FROM pg_stat_statements
WHERE dbid=(SELECT oid FROM pg_database WHERE datname=current_database())
AND query ILIKE '%runtime.checkpoints%' AND query NOT ILIKE '%pg_stat_statements%'""").fetchall()
    return {row[0]: {'query': row[1], 'calls': row[2], 'rows': row[3],
                    'totalExecMs': row[4], 'sharedBlocksHit': row[5],
                    'sharedBlocksRead': row[6]} for row in rows}


def delta(before, after):
    result = []
    for query_id, value in after.items():
        old = before.get(query_id, {})
        calls = value['calls'] - old.get('calls', 0)
        if calls:
            result.append({'queryId': query_id, 'query': value['query'], 'calls': calls,
                           'rows': value['rows'] - old.get('rows', 0),
                           'totalExecMs': value['totalExecMs'] - old.get('totalExecMs', 0),
                           'sharedBlocksHit': value['sharedBlocksHit'] - old.get('sharedBlocksHit', 0),
                           'sharedBlocksRead': value['sharedBlocksRead'] - old.get('sharedBlocksRead', 0)})
    return result


def seed(connection, unrelated):
    connection.execute('TRUNCATE runtime.checkpoints')
    connection.execute("""INSERT INTO runtime.checkpoints
(checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json)
VALUES ('profile:recovery','recovery','profile:session','profile:recovery-turn',
'committed',NULL,0,'{}')""")
    connection.execute("""INSERT INTO runtime.checkpoints
(checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json)
SELECT 'profile:wait:'||n,'wait','profile:session','profile:turn:'||n,
'waiting','runtime_job',n,'{}' FROM generate_series(1,%s) n""", (unrelated,))
    connection.execute('ANALYZE runtime.checkpoints')


def measure(connection, unrelated):
    seed(connection, unrelated)
    before = statement_snapshot(connection)
    offset = 0
    pages = 0
    returned = 0
    response_bytes = 0
    started = time.perf_counter()
    found = False
    while not found:
        rows = connection.execute(QUERY, ('profile:session', 100, offset)).fetchall()
        pages += 1
        returned += len(rows)
        response_bytes += len(json.dumps(rows, default=str, separators=(',', ':')).encode())
        found = any(row[1] == 'recovery' for row in rows)
        if not rows or found:
            break
        offset += len(rows)
    elapsed_ms = (time.perf_counter() - started) * 1000
    after = statement_snapshot(connection)
    return {'unrelatedCheckpoints': unrelated, 'pages': pages, 'rowsReturned': returned,
            'serializedRowBytes': response_bytes, 'foundRecovery': found,
            'elapsedMs': elapsed_ms, 'pgStatStatementsBefore': before,
            'pgStatStatementsAfter': after, 'pgStatStatements': delta(before, after)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if args.output.exists():
        raise FileExistsError(args.output)
    import psycopg
    url = os.environ['CENTAERIS_TEST_POSTGRES_URL']
    with psycopg.connect(url, autocommit=True) as connection:
        results = [measure(connection, size) for size in (0, 100, 1000, 10_000)]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps({
        'measurementBoundary': 'persistent-client SQL probe; excludes RuntimeStore per-page connections and restart semantics',
        'pageSize': 100,
        'results': results,
    }, indent=2), encoding='utf-8')
    print(args.output)


if __name__ == '__main__':
    main()
