"""Read-only metrics tests; no Docker, PostgreSQL, or host cgroup access."""
import copy
import io
import json
from pathlib import Path
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

from workspace_metrics import (CgroupSampler, DockerProbe, OwnedUsageReader,
    ProviderUsageAccumulator, TrialMetricsSampler, unknown_memory, unknown_usage)

RUNTIME = 'a' * 64
TASK = 'b' * 64
PG = 'c' * 64


def usage_row(sequence, **usage):
    return {'sessionId': 'session_1', 'agentRunId': 'run_1', 'turnId': f'turn_{sequence}',
            'agentRunSequence': sequence, 'sequence': sequence + 100,
            'inputTokens': 100, 'outputTokens': 10, 'promptCacheHitTokens': 75,
            'promptCacheMissTokens': 25, **usage}


class UsageTests(unittest.TestCase):
    def test_unknown_is_not_zero_and_partial_provider_fields_remain_unknown(self):
        counter = ProviderUsageAccumulator('session_1', 'run_1')
        self.assertEqual(counter.snapshot(), unknown_usage())
        counter.apply_page([])
        self.assertEqual(counter.snapshot()['requests'], 0)
        self.assertIsNone(counter.snapshot()['inputTokens'])
        self.assertFalse(counter.snapshot()['complete'])
        counter.apply_page([usage_row(4), usage_row(9, promptCacheMissTokens=None)])
        state = counter.snapshot()
        self.assertEqual((state['requests'], state['inputTokens'], state['outputTokens']), (2, 200, 20))
        self.assertEqual(state['promptCacheHitTokens'], 150)
        self.assertIsNone(state['promptCacheMissTokens'])
        self.assertIsNone(state['promptCacheHitRate'])
        self.assertFalse(state['complete'])
        self.assertEqual(counter.cursor, 9)

    def test_pages_overlap_without_double_count_and_rate_uses_reported_cache_tokens(self):
        counter = ProviderUsageAccumulator('session_1', 'run_1')
        page = [usage_row(i) for i in range(1, 33)]
        counter.apply_page(page)
        self.assertFalse(counter.snapshot()['complete'])
        counter.apply_page([usage_row(31), usage_row(32), usage_row(37)])
        state = counter.snapshot()
        self.assertEqual(state['requests'], 33)
        self.assertEqual(state['inputTokens'], 3300)
        self.assertEqual(state['promptCacheHitRate'], 0.75)
        self.assertTrue(state['complete'])
        counter.apply_page([])
        self.assertEqual(counter.snapshot(), state)

    def test_bad_scope_order_and_oversized_page_leave_cursor_untouched(self):
        counter = ProviderUsageAccumulator('session_1', 'run_1')
        for page in ([usage_row(1, sessionId='other')], [usage_row(1, agentRunId='other')],
                     [usage_row(3), usage_row(2)], [usage_row(1)] * 33):
            with self.assertRaises(ValueError):
                counter.apply_page(page)
            self.assertEqual(counter.cursor, 0)
            self.assertEqual(counter.snapshot(), unknown_usage())

    def test_zero_cache_denominator_is_unknown_and_overflow_never_rounds(self):
        counter = ProviderUsageAccumulator('session_1', 'run_1')
        counter.apply_page([usage_row(1, promptCacheHitTokens=0, promptCacheMissTokens=0)])
        self.assertIsNone(counter.snapshot()['promptCacheHitRate'])
        with self.assertRaises(ValueError):
            counter.apply_page([usage_row(2, inputTokens=2**53)])
        self.assertEqual(counter.cursor, 1)
        large = ProviderUsageAccumulator('session_1', 'run_1')
        large.apply_page([usage_row(1, inputTokens=2**53-1), usage_row(2)])
        self.assertIsNone(large.snapshot()['inputTokens'])
        self.assertFalse(large.snapshot()['complete'])


class FakeProbe:
    def __init__(self):
        self.calls = []

    def container(self, container_id):
        self.calls.append(container_id)
        return {'id': container_id, 'pid': 101 if container_id == RUNTIME else 102,
                'startedAt': '2026-10-17T01:00:00Z', 'running': True}


class CgroupTests(unittest.TestCase):
    def fixture(self, root):
        proc, cgroups = root / 'proc', root / 'cgroup'
        for container, pid in ((RUNTIME, 101), (TASK, 102)):
            process = proc / str(pid)
            process.mkdir(parents=True)
            name = f'docker-{container}.scope'
            (process / 'cgroup').write_text(f'0::/system.slice/{name}\n')
            (process / 'status').write_text('Name:\tpython\nRssAnon:\t123 kB\n')
            # Fields after comm start at field 3; field 22 is the starttime.
            (process / 'stat').write_text(f'{pid} (name with spaces) S ' + '0 '*18 + '999 0\n')
            group = cgroups / 'system.slice' / name
            group.mkdir(parents=True)
            for filename, value in {'memory.current': '1000', 'memory.peak': '2000',
                'memory.max': '536870912', 'memory.events': 'low 0\noom 1\noom_kill 1\n'}.items():
                (group / filename).write_text(value)
        return proc, cgroups

    def test_exact_host_cgroup_memory_and_rss_without_task_exec(self):
        with tempfile.TemporaryDirectory() as tmp:
            proc, cgroup = self.fixture(Path(tmp))
            probe = FakeProbe()
            sampler = CgroupSampler(probe, proc_root=proc, cgroup_root=cgroup)
            value = sampler.sample(RUNTIME, TASK)
            self.assertEqual(value, {'runtimeCurrentBytes': 1000, 'runtimePeakBytes': 2000,
                'runtimeAnonBytes': 123*1024, 'taskCurrentBytes': 1000, 'taskPeakBytes': 2000,
                'taskLimitBytes': 536870912, 'runtimeOomKills': 1, 'taskOomKills': 1})
            self.assertEqual(probe.calls, [RUNTIME, TASK])

    def test_shared_root_path_escape_and_other_container_cgroup_are_unknown(self):
        for path in ('/', '/../outside', '/system.slice/docker-'+'d'*64+'.scope'):
            with self.subTest(path=path), tempfile.TemporaryDirectory() as tmp:
                proc, cgroup = self.fixture(Path(tmp))
                (proc / '101/cgroup').write_text(f'0::{path}\n')
                state = CgroupSampler(FakeProbe(), proc_root=proc, cgroup_root=cgroup).sample(RUNTIME, TASK)
                self.assertIsNone(state['runtimeCurrentBytes'])
                self.assertEqual(state['taskCurrentBytes'], 1000)

    def test_missing_counters_unlimited_memory_and_pid_change_are_unknown(self):
        with tempfile.TemporaryDirectory() as tmp:
            proc, cgroup = self.fixture(Path(tmp))
            runtime = cgroup / 'system.slice' / f'docker-{RUNTIME}.scope'
            task = cgroup / 'system.slice' / f'docker-{TASK}.scope'
            (runtime / 'memory.peak').unlink()
            (task / 'memory.max').write_text('max')
            sampler = CgroupSampler(FakeProbe(), proc_root=proc, cgroup_root=cgroup)
            state = sampler.sample(RUNTIME, TASK)
            self.assertIsNone(state['runtimePeakBytes'])
            self.assertIsNone(state['taskLimitBytes'])
            self.assertEqual(state['runtimeCurrentBytes'], 1000)
            original = sampler._start_time
            reads = []
            def changed(pid):
                reads.append(pid)
                return 'different' if len(reads) == 2 else original(pid)
            with patch.object(sampler, '_start_time', side_effect=changed):
                self.assertIsNone(sampler.sample(RUNTIME, None)['runtimeCurrentBytes'])

    def test_cgroup_symlink_cannot_alias_an_unrelated_task(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            proc, cgroup = self.fixture(root)
            alias = cgroup / 'alias'
            try:
                alias.symlink_to(cgroup / 'system.slice' / f'docker-{TASK}.scope', target_is_directory=True)
            except OSError:
                self.skipTest('account cannot create directory symlinks')
            # The lexical Docker ID may be correct while the actual target is not.
            original = cgroup / 'system.slice' / f'docker-{RUNTIME}.scope'
            import shutil
            self.assertTrue(original.resolve().is_relative_to(root.resolve()))
            shutil.rmtree(original)
            original.symlink_to(alias, target_is_directory=True)
            value = CgroupSampler(FakeProbe(), proc_root=proc, cgroup_root=cgroup).sample(RUNTIME, TASK)
            self.assertIsNone(value['runtimeCurrentBytes'])
            self.assertEqual(value['taskCurrentBytes'], 1000)

    def test_probe_asks_only_for_selected_inspection_fields(self):
        inspected = {'id': RUNTIME, 'pid': 101, 'startedAt': '2026-10-17T01:00:00Z', 'running': True}
        with patch('workspace_metrics.subprocess.run', return_value=type('Result', (), {
                'stdout': json.dumps(inspected), 'returncode': 0})()) as command:
            probe = DockerProbe('/var/run/docker.sock')
            self.assertEqual(probe.container(RUNTIME), inspected)
            args = command.call_args.args[0]
            self.assertEqual(args[-1], RUNTIME)
            self.assertIn('.State.Pid', args[args.index('--format')+1])
            self.assertEqual(args[0:3], ['docker', '--host', 'unix:///var/run/docker.sock'])
        with self.assertRaises(ValueError):
            probe.container('shared-name')


class ReaderTests(unittest.TestCase):
    def fixture(self, root):
        identity = {'schema': 'centaeris.workspaceBenchmarkFixture.v1',
            'fixtureId': 'tbworkspace'+'a'*10, 'databaseName': 'tbworkspace'+'a'*10,
            'databaseUser': 'tbworkspace'+'a'*10, 'sourceRevision': 'b'*40, 'sourceTree': 'c'*40,
            'dockerHostId': 'daemon-owned', 'dockerSocket': '/var/run/docker.sock'}
        path = root / 'fixture.json'
        path.write_text(json.dumps(identity))
        return path, identity

    def test_owned_database_reader_projects_only_numeric_page(self):
        with tempfile.TemporaryDirectory() as tmp:
            path, identity = self.fixture(Path(tmp))
            responses = [type('Result', (), {'stdout': 'daemon-owned', 'returncode': 0})(),
                type('Result', (), {'stdout': json.dumps({'id': PG, 'project': identity['fixtureId']}), 'returncode': 0})(),
                type('Result', (), {'stdout': json.dumps({'owned': True})+'\n'+json.dumps(usage_row(5))+'\n', 'returncode': 0})()]
            with patch('workspace_metrics.subprocess.run', side_effect=responses) as run:
                reader = OwnedUsageReader(path, PG)
                self.assertEqual(reader.read('session_1', 'run_1', 3), [usage_row(5)])
                query = run.call_args.kwargs['input']
                self.assertIn('BEGIN READ ONLY', query)
                self.assertIn('LIMIT 32', query)
                self.assertIn("payload->>'type'='provider_usage'", query)
                self.assertIn("agent_run_sequence > :'after_seq'::bigint", query)
                args = run.call_args.args[0]
                self.assertIn('--set=session_id=session_1', args)
                self.assertIn('--set=run_id=run_1', args)
                self.assertEqual(args[3:6], ['exec', '-i', PG])

    def test_wrong_compose_owner_or_database_proof_is_rejected(self):
        for owner, owned in (('foreign', True), ('tbworkspace'+'a'*10, False)):
            with self.subTest(owner=owner), tempfile.TemporaryDirectory() as tmp:
                path, identity = self.fixture(Path(tmp))
                values = [type('Result', (), {'stdout': 'daemon-owned', 'returncode': 0})(),
                    type('Result', (), {'stdout': json.dumps({'id': PG, 'project': owner}), 'returncode': 0})(),
                    type('Result', (), {'stdout': json.dumps({'owned': owned})+'\n', 'returncode': 0})()]
                with patch('workspace_metrics.subprocess.run', side_effect=values):
                    with self.assertRaises(ValueError):
                        OwnedUsageReader(path, PG).read('session_1', 'run_1', 0)


class SamplerTests(unittest.TestCase):
    def test_failures_are_unknown_then_retry_resumes_without_duplicate_usage(self):
        class Reader:
            def __init__(self): self.cursors = []
            def read(self, session, run, cursor):
                self.cursors.append(cursor)
                if len(self.cursors) == 2: raise OSError('synthetic unavailable')
                return [usage_row(3)] if cursor == 0 else [usage_row(8)]
        reader, observed = Reader(), []
        memory = type('Memory', (), {'sample': lambda *args: unknown_memory()})()
        sampler = TrialMetricsSampler('session_1', 'run_1', RUNTIME, lambda: TASK,
            observed.append, usage_reader=reader, memory_sampler=memory)
        sampler.sample_once(); sampler.sample_once(); sampler.sample_once()
        self.assertEqual(reader.cursors, [0, 3, 3])
        self.assertEqual(observed[0]['usage']['requests'], 1)
        self.assertEqual(observed[1]['usage'], unknown_usage())
        self.assertEqual(observed[2]['usage']['requests'], 2)
        self.assertEqual(observed[2]['memory'], unknown_memory())
        self.assertTrue(observed[2]['observedAt'].endswith('Z'))

    def test_task_binding_change_does_not_show_another_instances_memory(self):
        ids = iter([TASK, 'd' * 64, None])
        calls = []
        class Memory:
            def sample(self, runtime, task):
                calls.append(task)
                return unknown_memory()
        sampler = TrialMetricsSampler('session_1', 'run_1', RUNTIME, lambda: next(ids),
            lambda value: None, memory_sampler=Memory())
        sampler.sample_once(); sampler.sample_once(); sampler.sample_once()
        self.assertEqual(calls, [TASK, None, None])

    def test_slow_optional_reader_never_blocks_trial_or_shutdown(self):
        entered, unblock = threading.Event(), threading.Event()
        class Reader:
            def read(self, *args):
                entered.set(); unblock.wait(2); return []
        memory = type('Memory', (), {'sample': lambda *args: unknown_memory()})()
        sampler = TrialMetricsSampler('session_1', 'run_1', RUNTIME, lambda: TASK,
            lambda value: None, usage_reader=Reader(), memory_sampler=memory)
        before = time.monotonic(); sampler.start()
        self.assertLess(time.monotonic()-before, 0.2)
        self.assertTrue(entered.wait(1))
        before = time.monotonic(); sampler.close()
        self.assertLess(time.monotonic()-before, 0.5)
        unblock.set()


if __name__ == '__main__':
    unittest.main()
