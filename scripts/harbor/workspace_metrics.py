"""Optional, bounded observations for the Workspace benchmark controller.

These observations never authorize execution or cleanup. Provider counts cover
committed provider_usage reports in the exact main AgentRun, not transport retry
attempts, in-flight requests, context estimates, or child AgentRuns. No historical
message, model request, tool output, container environment, or credential is read.
"""
from __future__ import annotations

from datetime import datetime, timezone
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import threading
from typing import Protocol

SAFE_INTEGER = 2**53 - 1
PAGE_LIMIT = 32
USAGE_FIELDS = ('inputTokens', 'outputTokens', 'promptCacheHitTokens', 'promptCacheMissTokens')
MEMORY_FIELDS = ('runtimeCurrentBytes', 'runtimePeakBytes', 'runtimeAnonBytes',
    'taskCurrentBytes', 'taskPeakBytes', 'taskLimitBytes', 'runtimeOomKills', 'taskOomKills')
ROW_FIELDS = {'sessionId', 'agentRunId', 'turnId', 'agentRunSequence', 'sequence', *USAGE_FIELDS}


def unknown_usage():
    return {'source': 'committedProviderUsage', 'scope': 'mainAgent', 'requests': None,
            **{key: None for key in USAGE_FIELDS}, 'promptCacheHitRate': None, 'complete': False}


def unknown_memory():
    return {key: None for key in MEMORY_FIELDS}


def _integer(value, minimum=0):
    if type(value) is not int or not minimum <= value <= SAFE_INTEGER:
        raise ValueError('metric_integer_invalid')
    return value


def _identity(value):
    if not isinstance(value, str) or not re.fullmatch(r'[A-Za-z0-9_.:-]{1,160}', value):
        raise ValueError('metric_identity_invalid')
    return value


def _container(value):
    if not isinstance(value, str) or not re.fullmatch(r'[0-9a-f]{64}', value):
        raise ValueError('metric_container_identity_invalid')
    return value


class ProviderUsageAccumulator:
    """Scalar-only per-Run fold with an authoritative numeric sequence cursor.

    A field's sum is known only if every consumed report supplied that field.
    complete means field coverage of committed reports plus a caught-up page;
    it does not imply every real request has already committed a usage report.
    """
    def __init__(self, session_id, agent_run_id):
        self.session_id = _identity(session_id)
        self.agent_run_id = _identity(agent_run_id)
        self.cursor = 0
        self._requests = 0
        self._totals = dict.fromkeys(USAGE_FIELDS, 0)
        self._known = dict.fromkeys(USAGE_FIELDS, True)
        self._observed = False
        self._caught_up = False

    def apply_page(self, rows):
        if not isinstance(rows, list) or len(rows) > PAGE_LIMIT:
            raise ValueError('usage_page_invalid')
        validated = []
        previous = 0
        for row in rows:
            if not isinstance(row, dict) or set(row) != ROW_FIELDS:
                raise ValueError('usage_row_fields_invalid')
            if row['sessionId'] != self.session_id or row['agentRunId'] != self.agent_run_id:
                raise ValueError('usage_row_scope_mismatch')
            _identity(row['turnId'])
            sequence = _integer(row['agentRunSequence'], 1)
            _integer(row['sequence'], 1)
            if sequence <= previous:
                raise ValueError('usage_row_sequence_invalid')
            previous = sequence
            for key in USAGE_FIELDS:
                if row[key] is not None:
                    _integer(row[key])
            validated.append(row)
        # Only validated pages advance the cursor or make partial totals visible.
        for row in validated:
            if row['agentRunSequence'] <= self.cursor:
                continue
            self.cursor = row['agentRunSequence']
            self._requests += 1
            for key in USAGE_FIELDS:
                if row[key] is None:
                    self._known[key] = False
                elif self._known[key]:
                    total = self._totals[key] + row[key]
                    if total > SAFE_INTEGER:
                        self._known[key] = False
                    else:
                        self._totals[key] = total
        self._observed = True
        self._caught_up = len(rows) < PAGE_LIMIT

    def snapshot(self):
        if not self._observed:
            return unknown_usage()
        values = {key: self._totals[key] if self._requests and self._known[key] else None
                  for key in USAGE_FIELDS}
        complete = bool(self._requests and self._caught_up and all(self._known.values()))
        denominator = (values['promptCacheHitTokens'] or 0) + (values['promptCacheMissTokens'] or 0)
        rate = values['promptCacheHitTokens'] / denominator if complete and denominator else None
        return {'source': 'committedProviderUsage', 'scope': 'mainAgent', 'requests': self._requests,
                **values, 'promptCacheHitRate': rate, 'complete': complete}


class UsagePageReader(Protocol):
    def read(self, session_id: str, agent_run_id: str, after_sequence: int) -> list[dict]: ...


class DockerProbe:
    def __init__(self, socket_path, *, timeout=2):
        if (not isinstance(socket_path, str) or not socket_path.startswith('/')
                or socket_path.startswith('//') or str(PurePosixPath(socket_path)) != socket_path
                or '..' in socket_path.split('/') or '\\' in socket_path
                or any(ord(c) < 33 for c in socket_path) or ':' in socket_path):
            raise ValueError('metric_docker_socket_invalid')
        self.command = ['docker', '--host', 'unix://' + socket_path]
        self.timeout = timeout

    def _inspect(self, container_id, template):
        _container(container_id)
        try:
            result = subprocess.run(self.command + ['inspect', '--type', 'container', '--format', template, container_id],
                check=True, capture_output=True, text=True, timeout=self.timeout, stdin=subprocess.DEVNULL)
            if len(result.stdout) > 4096:
                raise ValueError('metric_docker_observation_too_large')
            value = json.loads(result.stdout)
        except (OSError, subprocess.SubprocessError, ValueError):
            raise ValueError('metric_docker_observation_unavailable') from None
        if not isinstance(value, dict) or value.get('id') != container_id:
            raise ValueError('metric_docker_identity_mismatch')
        return value

    def container(self, container_id):
        value = self._inspect(container_id,
            '{"id":{{json .Id}},"pid":{{json .State.Pid}},"startedAt":{{json .State.StartedAt}},"running":{{json .State.Running}}}')
        if (set(value) != {'id', 'pid', 'startedAt', 'running'} or type(value['pid']) is not int
                or value['pid'] < 0 or type(value['running']) is not bool
                or not isinstance(value['startedAt'], str) or len(value['startedAt']) > 64):
            raise ValueError('metric_docker_state_invalid')
        return value

    def host_id(self):
        try:
            result = subprocess.run(self.command + ['info', '--format', '{{.ID}}'], check=True,
                capture_output=True, text=True, timeout=self.timeout, stdin=subprocess.DEVNULL)
            value = result.stdout.strip()
        except (OSError, subprocess.SubprocessError):
            raise ValueError('metric_docker_host_unavailable') from None
        if not re.fullmatch(r'[A-Za-z0-9:_-]{1,128}', value):
            raise ValueError('metric_docker_host_invalid')
        return value

    def project(self, container_id):
        # Read only this known non-secret owner label, never Config or Env JSON.
        value = self._inspect(container_id,
            '{"id":{{json .Id}},"project":{{json (index .Config.Labels "com.docker.compose.project")}}}')
        if set(value) != {'id', 'project'} or not isinstance(value['project'], str):
            raise ValueError('metric_docker_project_invalid')
        return value['project']


class CgroupSampler:
    def __init__(self, probe, *, proc_root=Path('/proc'), cgroup_root=Path('/sys/fs/cgroup')):
        self.probe = probe
        self.proc_root = Path(proc_root)
        self.cgroup_root = Path(cgroup_root)

    @staticmethod
    def _read(path, limit):
        if path.is_symlink():
            raise ValueError('metric_file_link_rejected')
        with path.open('r', encoding='ascii') as handle:
            value = handle.read(limit + 1)
        if len(value) > limit:
            raise ValueError('metric_file_too_large')
        return value

    def _start_time(self, pid):
        value = self._read(self.proc_root / str(pid) / 'stat', 4096)
        fields = value.rsplit(')', 1)[1].split()
        if len(fields) < 20 or not fields[19].isdigit():
            raise ValueError('metric_process_identity_unavailable')
        return fields[19]

    def _number(self, path):
        try:
            value = self._read(path, 64).strip()
            if not re.fullmatch(r'[0-9]{1,16}', value):
                return None
            return _integer(int(value))
        except (OSError, ValueError):
            return None

    def _one(self, container_id, *, anon=False):
        _container(container_id)
        observed = self.probe.container(container_id)
        if not observed['running'] or observed['pid'] <= 0:
            raise ValueError('metric_container_not_running')
        pid = observed['pid']
        before = self._start_time(pid)
        content = self._read(self.proc_root / str(pid) / 'cgroup', 8192)
        unified = [line[3:] for line in content.splitlines() if line.startswith('0::')]
        if len(unified) != 1:
            raise ValueError('metric_cgroup_v2_unavailable')
        path = unified[0]
        relative = PurePosixPath(path)
        if (not path.startswith('/') or str(relative) != path or '..' in relative.parts
                or '\\' in path or len(relative.parts) < 2 or any(ord(c) < 32 for c in path)):
            raise ValueError('metric_cgroup_path_invalid')
        if not any(part in {container_id, f'docker-{container_id}.scope'} for part in relative.parts):
            raise ValueError('metric_cgroup_container_mismatch')
        root = self.cgroup_root.resolve(strict=True)
        component = root
        for name in relative.parts[1:]:
            component = component / name
            if component.is_symlink():
                raise ValueError('metric_cgroup_link_rejected')
        group = (root / path.lstrip('/')).resolve(strict=True)
        if group == root or not group.is_relative_to(root):
            raise ValueError('metric_cgroup_scope_invalid')
        values = {'current': self._number(group / 'memory.current'),
                  'peak': self._number(group / 'memory.peak'),
                  'limit': self._number(group / 'memory.max'), 'kills': None, 'anon': None}
        try:
            counters = dict(line.split() for line in self._read(group / 'memory.events', 4096).splitlines())
            if 'oom_kill' in counters:
                values['kills'] = _integer(int(counters['oom_kill']))
        except (OSError, ValueError):
            pass
        if anon:
            try:
                match = re.search(r'^RssAnon:\s+([0-9]+) kB$',
                    self._read(self.proc_root / str(pid) / 'status', 65536), re.MULTILINE)
                if match:
                    values['anon'] = _integer(int(match[1]) * 1024)
            except (OSError, ValueError):
                pass
        if before != self._start_time(pid):
            raise ValueError('metric_process_generation_changed')
        return values

    def sample(self, runtime_container_id, task_container_id):
        result = unknown_memory()
        for prefix, container_id in (('runtime', runtime_container_id), ('task', task_container_id)):
            if container_id is None:
                continue
            try:
                value = self._one(container_id, anon=prefix == 'runtime')
                result[prefix + 'CurrentBytes'] = value['current']
                result[prefix + 'PeakBytes'] = value['peak']
                result[prefix + 'OomKills'] = value['kills']
                result['runtimeAnonBytes' if prefix == 'runtime' else 'taskLimitBytes'] = value['anon' if prefix == 'runtime' else 'limit']
            except (OSError, ValueError, IndexError, KeyError):
                pass
        return result


def _numeric_sql(name):
    text = f"e.payload->'payload'->>'{name}'"
    value = f"e.payload->'payload'->'{name}'"
    return (f"CASE WHEN jsonb_typeof({value})='number' THEN CASE WHEN length({text})<=16 "
            f"AND ({text})~'^(0|[1-9][0-9]{{0,15}})$' THEN CASE WHEN ({text})::numeric<={SAFE_INTEGER} "
            f"THEN ({text})::bigint ELSE NULL END ELSE NULL END ELSE NULL END")


class OwnedUsageReader:
    """Numeric projection from the already-owned fixture PostgreSQL database.

    Credentials are not read. psql uses the container's local Unix socket as its
    configured owner role. Every read verifies the exact Compose project label,
    actual database/user and signed benchmark Run binding in a read-only transaction.
    """
    def __init__(self, fixture_identity_path, postgres_container_id, *, timeout=2):
        path = Path(fixture_identity_path)
        if path.is_symlink() or not path.is_file() or path.stat().st_size > 16384:
            raise ValueError('usage_fixture_invalid')
        identity = json.loads(path.read_text(encoding='utf-8'))
        required = {'schema', 'fixtureId', 'databaseName', 'databaseUser', 'sourceRevision',
                    'sourceTree', 'dockerHostId', 'dockerSocket'}
        if (not isinstance(identity, dict) or set(identity) != required
                or identity['schema'] != 'centaeris.workspaceBenchmarkFixture.v1'
                or not re.fullmatch(r'tbworkspace[0-9a-f]{10}', str(identity['fixtureId']))
                or identity['databaseName'] != identity['fixtureId'] or identity['databaseUser'] != identity['fixtureId']):
            raise ValueError('usage_fixture_invalid')
        self.identity = identity
        self.postgres_id = _container(postgres_container_id)
        self.probe = DockerProbe(identity['dockerSocket'], timeout=timeout)
        self.timeout = timeout
        self._host_verified = False

    @staticmethod
    def _scope_sql(parameter=lambda name: ":'" + name + "'"):
        """One ownership predicate for psql and real PostgreSQL behavior tests."""
        return f"""SELECT (current_database()={parameter('expected_db')} AND current_user={parameter('expected_user')} AND EXISTS(
    SELECT 1 FROM app_core_agentrun r JOIN app_core_sessionenvironment env ON env.id=r.environment_id
    JOIN app_core_agentrunauthorization a ON a.agent_run_id=r.id
    WHERE r.id={parameter('run_id')} AND r.session_id={parameter('session_id')} AND env.session_id=r.session_id
    AND env.benchmark_profile IS NOT NULL AND a.payload->>'imageCapability'='benchmark_task_v1'
    AND env.network=env.benchmark_profile->>'network'
    AND (a.payload->>'schema'='workspace.agent_run_authorization.v2'
        OR (a.payload->>'schema'='workspace.agent_run_authorization.v3' AND a.payload->>'network'=env.network))
    AND a.payload->'benchmarkProfile'=env.benchmark_profile
    AND a.payload->>'benchmarkProfileDigest'=env.benchmark_profile_digest))"""

    def read(self, session_id, agent_run_id, after_sequence):
        _identity(session_id); _identity(agent_run_id); _integer(after_sequence)
        if not self._host_verified:
            if self.probe.host_id() != self.identity['dockerHostId']:
                raise ValueError('usage_docker_host_mismatch')
            self._host_verified = True
        if self.probe.project(self.postgres_id) != self.identity['fixtureId']:
            raise ValueError('usage_database_owner_mismatch')
        fields = ',\n'.join(f"'{name}', {_numeric_sql(name)}" for name in USAGE_FIELDS)
        query = """BEGIN READ ONLY;
SET LOCAL statement_timeout='750ms';
SET LOCAL lock_timeout='250ms';
""" + self._scope_sql() + """ AS metrics_owned \\gset
\\if :metrics_owned
SELECT json_build_object('owned',true);
SELECT json_build_object(
    'sessionId', CASE WHEN length(e.payload->>'sessionId')<=160 THEN e.payload->>'sessionId' ELSE NULL END,
    'agentRunId', CASE WHEN length(e.payload->>'agentRunId')<=160 THEN e.payload->>'agentRunId' ELSE NULL END,
    'turnId', CASE WHEN length(e.payload->>'turnId')<=160 THEN e.payload->>'turnId' ELSE NULL END,
    'agentRunSequence',e.agent_run_sequence,'sequence',e.sequence,
""" + fields + """
) FROM app_core_sessionevent e
WHERE e.session_id=:'session_id' AND e.agent_run_id=:'run_id' AND e.session_level=FALSE
    AND e.payload->>'type'='provider_usage' AND e.agent_run_sequence > :'after_seq'::bigint
ORDER BY e.agent_run_sequence LIMIT 32;
\\else
SELECT json_build_object('owned',false);
\\endif
COMMIT;
"""
        command = self.probe.command + ['exec', '-i', self.postgres_id, 'psql', '-X', '-qAt', '--no-password',
            '--set=ON_ERROR_STOP=1', '--username=' + self.identity['databaseUser'], '--dbname=' + self.identity['databaseName'],
            '--set=expected_db=' + self.identity['databaseName'], '--set=expected_user=' + self.identity['databaseUser'],
            '--set=session_id=' + session_id, '--set=run_id=' + agent_run_id, '--set=after_seq=' + str(after_sequence)]
        try:
            result = subprocess.run(command, input=query, check=True, capture_output=True,
                text=True, timeout=self.timeout)
            if len(result.stdout) > 65536:
                raise ValueError('usage_page_too_large')
            values = [json.loads(line) for line in result.stdout.splitlines() if line.strip()]
        except (OSError, subprocess.SubprocessError, ValueError):
            raise ValueError('usage_reader_unavailable') from None
        if not values or values[0] != {'owned': True} or len(values) > PAGE_LIMIT + 1:
            raise ValueError('usage_database_scope_not_verified')
        return values[1:]


class TrialMetricsSampler:
    """One bounded optional worker per active trial; task execution never waits.

    Callbacks should enqueue nonblocking projection updates. A blocked reader or
    callback can occupy only this daemon thread, and close never waits for it
    indefinitely. A failed observation publishes unknown fields, not zeroes.
    """
    def __init__(self, session_id, agent_run_id, runtime_container_id, task_container_id,
                 callback, *, usage_reader=None, memory_sampler=None, interval_seconds=10):
        self.counter = ProviderUsageAccumulator(session_id, agent_run_id)
        self.runtime_id = _container(runtime_container_id)
        self.task_id = task_container_id
        self._owned_task_id = None
        self.callback = callback
        self.reader = usage_reader
        self.memory = memory_sampler
        if type(interval_seconds) not in (int, float) or not 10 <= interval_seconds <= 300:
            raise ValueError('metric_interval_invalid')
        self.interval = interval_seconds
        self._stop = threading.Event()
        self._thread = None

    def sample_once(self):
        usage, memory = unknown_usage(), unknown_memory()
        if self.reader is not None:
            try:
                rows = self.reader.read(self.counter.session_id, self.counter.agent_run_id, self.counter.cursor)
                self.counter.apply_page(rows)
                usage = self.counter.snapshot()
            except Exception:
                pass
        if self.memory is not None:
            try:
                selected = self.task_id()
                if selected is not None:
                    _container(selected)
                    if self._owned_task_id is None:
                        self._owned_task_id = selected
                    elif selected != self._owned_task_id:
                        selected = None
                memory = self.memory.sample(self.runtime_id, selected)
            except Exception:
                pass
        value = {'observedAt': datetime.now(timezone.utc).isoformat().replace('+00:00', 'Z'),
                 'usage': usage, 'memory': memory}
        if not self._stop.is_set():
            try:
                self.callback(value)
            except Exception:
                pass
        return value

    def _run(self):
        while not self._stop.is_set():
            self.sample_once()
            self._stop.wait(self.interval)

    def start(self):
        if self._thread is not None:
            raise ValueError('metric_sampler_already_started')
        self._thread = threading.Thread(target=self._run, name='benchmark-metrics', daemon=True)
        self._thread.start()

    def close(self):
        self._stop.set()
        if self._thread is not None and self._thread is not threading.current_thread():
            self._thread.join(timeout=0.1)
