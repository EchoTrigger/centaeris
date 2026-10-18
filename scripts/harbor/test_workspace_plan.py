"""Behavioral policy tests; no Docker, provider, or database access."""
import asyncio
import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from workspace_plan import (
    Admission, CandidateBinding, Resources, canonical_digest, evaluate_pilot,
    load_plan, load_task_resources, make_binding, require_full_review, TaskResources, validate_plan,
)

HERE = Path(__file__).resolve().parent


def plan():
    return load_plan(HERE / 'workspace-pilot-tasks.json')


def binding():
    return CandidateBinding('a' * 40, 'b' * 64, canonical_digest(plan()), 'd' * 64)


def results():
    return [dict(trialId=f'trial-{i}', taskId=task, attempt=1,
                 verifierCompleted=True, reward=0, infrastructureError=None,
                 agentOutcome={'status': 'completed', 'error': None},
                 verifierOutcome={'status': 'completed', 'error': None},
                 cleanupOutcome={'status': 'absent', 'error': None},
                 severeIssue=None, cleanupState='absent', resourceDebit=0)
            for i, task in enumerate(plan()['taskIds'])]


class PlanTests(unittest.TestCase):
    def test_verifier_outcomes_accept_exact_live_and_historical_states_without_rewriting(self):
        for status, error in (('notStarted', None), ('running', None), ('unknown', None),
                              ('completed', None), ('failed', 'benchmark_download_path_missing')):
            with self.subTest(status=status):
                outcome = {'status': status, 'error': error}
                batch = results()
                batch[0]['verifierOutcome'] = outcome
                if status != 'completed':
                    batch[0]['verifierCompleted'], batch[0]['reward'] = False, None
                original = copy.deepcopy(batch)
                report = evaluate_pilot(plan(), binding(), batch)
                self.assertEqual(report['status'], 'readyForReview' if status == 'completed' else 'blocked')
                self.assertNotIn('invalid verifier outcome status', report['issues'])
                self.assertEqual(batch, original)
                self.assertEqual(report['results'], original)
                if status != 'completed':
                    approval = dict(schema='centaeris.workspaceFullReview.v1', decision='approveFull',
                        reviewer='maintainer', candidate=binding().to_dict(),
                        pilotEvidenceSha256=report['evidenceSha256'])
                    with self.assertRaisesRegex(ValueError, 'not ready'):
                        require_full_review(binding(), report, approval)
        for status in ('not_started', 'notstarted', 'passed', 'NotStarted'):
            with self.subTest(status=status):
                batch = results()
                batch[0]['verifierOutcome']['status'] = status
                self.assertIn('invalid verifier outcome status', evaluate_pilot(plan(), binding(), batch)['issues'])
        for phase in ('agent', 'cleanup'):
            with self.subTest(phase=phase):
                batch = results()
                batch[0][phase + 'Outcome']['status'] = 'notStarted'
                self.assertIn(f'invalid {phase} outcome status', evaluate_pilot(plan(), binding(), batch)['issues'])

    def test_bounded_pilot_accepts_one_through_twenty_unique_tasks_once(self):
        for count in (1, 17, 20):
            with self.subTest(count=count):
                value = plan()
                value['taskIds'] = value['taskIds'][:count]
                self.assertIs(validate_plan(value, runnable=True), value)
        for count, attempts in ((0, 1), (21, 1), (17, 2), (17, True)):
            with self.subTest(count=count, attempts=attempts):
                value = plan()
                value.update(taskIds=[f'task-{i}' for i in range(count)], attempts=attempts)
                with self.assertRaises(ValueError):
                    validate_plan(value)

    def test_bounded_review_requires_every_exact_selected_task_once(self):
        for count in (1, 17, 20):
            with self.subTest(count=count):
                value = plan()
                value['taskIds'] = value['taskIds'][:count]
                candidate = CandidateBinding('a' * 40, 'b' * 64, canonical_digest(value), 'd' * 64)
                batch = results()[:count]
                self.assertEqual(evaluate_pilot(value, candidate, batch)['status'], 'readyForReview')
                for broken in (batch[:-1], batch + batch[:1],
                               [{**row, 'taskId': 'unselected'} if i == 0 else row
                                for i, row in enumerate(batch)]):
                    self.assertEqual(evaluate_pilot(value, candidate, broken)['status'], 'blocked')

    def test_full_review_requires_twenty_distinct_results_even_with_matching_approval(self):
        full = CandidateBinding('a' * 40, 'b' * 64, 'e' * 64, 'f' * 64)
        for count in (1, 17, 20):
            with self.subTest(count=count):
                report = evaluate_pilot(plan(), binding(), results())
                report['status'], report['issues'] = 'readyForReview', []
                report['results'] = results()[:count]
                if count == 20:
                    report['results'][-1]['taskId'] = report['results'][0]['taskId']
                report['evidenceSha256'] = canonical_digest({key: value for key, value in report.items()
                                                            if key != 'evidenceSha256'})
                approval = dict(schema='centaeris.workspaceFullReview.v1', decision='approveFull',
                                reviewer='maintainer', candidate=full.to_dict(),
                                pilotEvidenceSha256=report['evidenceSha256'])
                with self.assertRaisesRegex(ValueError, 'twenty distinct'):
                    require_full_review(full, report, approval)

    def test_outcome_structure_and_final_state_must_agree_with_result(self):
        changes = [
            ('agentOutcome', {'status': 'unknown', 'error': 'AgentTimeoutError'}),
            ('agentOutcome', {'status': 'failed', 'error': None}),
            ('agentOutcome', {'status': 'completed', 'error': 'AgentTimeoutError'}),
            ('agentOutcome', {'status': 'failed', 'error': 42}),
            ('agentOutcome', {'status': 'failed', 'error': 'x' * 257}),
            ('agentOutcome', {'status': 'failed', 'error': 'unsafe\nerror'}),
            ('verifierOutcome', {'status': 'unknown', 'error': None}),
            ('verifierOutcome', {'status': 'failed', 'error': 'downloadFailed'}),
            ('cleanupOutcome', {'status': 'pending', 'error': None}),
            ('cleanupOutcome', {'status': 'absent', 'error': None, 'unknown': True}),
        ]
        for field, outcome in changes:
            with self.subTest(field=field, outcome=outcome):
                batch = results()
                batch[0][field] = outcome
                self.assertEqual(evaluate_pilot(plan(), binding(), batch)['status'], 'blocked')
        batch = results()
        batch[0]['agentOutcome'] = {'status': 'failed', 'error': 'AgentTimeoutError'}
        batch[0]['cleanupOutcome']['error'] = 'environment_retirement_pending'
        self.assertEqual(evaluate_pilot(plan(), binding(), batch)['status'], 'readyForReview')
        del batch[0]['agentOutcome']
        self.assertEqual(evaluate_pilot(plan(), binding(), batch)['status'], 'blocked')

    def test_pilot_is_twenty_distinct_tasks_once_without_auto_full_run(self):
        value = plan()
        self.assertEqual(len(set(value['taskIds'])), 20)
        self.assertEqual(value['attempts'], 1)
        self.assertEqual(value['maxConcurrentTrials'], 2)
        self.assertEqual(value['model']['outputTokens'], 64000)
        self.assertEqual(value['model']['thinking'], 'high')
        self.assertFalse(value['fullRunAuthorized'])
        value['model']['outputTokens'] = None
        with self.assertRaisesRegex(ValueError, 'outputTokens'):
            validate_plan(value, runnable=True)

    def test_model_descriptor_accepts_distinct_providers_and_binds_all_settings(self):
        first = plan()
        second = copy.deepcopy(first)
        second['model'] = {'apiBase': 'https://models.example.org/v1',
            'credentialEnv': 'ALTERNATE_MODEL_KEY', 'id': 'another/reasoner',
            'thinking': None, 'contextTokens': 32768, 'outputTokens': 4096}
        self.assertIs(validate_plan(second, runnable=True), second)
        self.assertNotEqual(canonical_digest(first), canonical_digest(second))

    def test_model_descriptor_rejects_invalid_endpoints_and_budgets(self):
        for update in ({'apiBase': 'http://models.example.org/v1'},
                       {'apiBase': 'https://secret@models.example.org/v1'},
                       {'apiBase': 'https://models.example.org/v1?key=secret'},
                       {'apiBase': 'https://models.example.org/v1#secret'},
                       {'apiBase': 'https:///v1'}, {'apiBase': 42},
                       {'credentialEnv': 'INVALID-NAME'}, {'credentialEnv': 'key=value'},
                       {'id': ''}, {'thinking': ''}, {'thinking': True},
                       {'contextTokens': True}, {'contextTokens': 0},
                       {'outputTokens': True}, {'outputTokens': 500000},
                       {'extra': 'unknown'}):
            with self.subTest(update=update):
                value = plan()
                value['model'].update(update)
                with self.assertRaises(ValueError):
                    validate_plan(value, runnable=True)

    def test_rejects_duplicates_unknown_fields_and_silent_budget_changes(self):
        for change in (lambda p: p['taskIds'].__setitem__(1, p['taskIds'][0]),
                       lambda p: p.update(typo=True),
                       lambda p: p.update(attempts=5),
                       lambda p: p['model'].update(contextTokens=0)):
            value = plan()
            change(value)
            with self.assertRaises(ValueError):
                validate_plan(value)

    def test_configured_pilot_is_in_pinned_full_registry_and_full_is_445_trials(self):
        full_registry = json.loads((HERE / 'full-tasks.json').read_text(encoding='utf-8'))
        full_ids = [item['path'].removeprefix('tasks/') for item in full_registry['tasks']]
        self.assertTrue(set(plan()['taskIds']).issubset(full_ids))
        self.assertTrue(all(item['git_commit_id'] == plan()['dataset']['commit']
                            for item in full_registry['tasks']))
        full = plan()
        full['taskIds'] = full_ids
        full['attempts'] = 5
        validate_plan(full, runnable=True)
        self.assertEqual(len(full['taskIds']) * full['attempts'], 445)
        self.assertFalse(full['fullRunAuthorized'])
        with self.assertRaises(ValueError):
            evaluate_pilot(full, binding(), [])

    def test_full_registry_supports_explicit_single_attempt_plan(self):
        full = plan()
        registry = json.loads((HERE / 'full-tasks.json').read_text(encoding='utf-8'))
        full['taskIds'] = [item['path'].removeprefix('tasks/') for item in registry['tasks']]
        full['attempts'] = 1
        self.assertIs(validate_plan(full, runnable=True), full)
        self.assertEqual(len(full['taskIds']), 89)
        self.assertFalse(full['fullRunAuthorized'])

    def test_bindings_hash_real_adapter_and_resource_inventory(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / 'adapter.py'
            source.write_text('first', encoding='utf-8')
            value = plan()
            inventory = {task: TaskResources(Resources(2048, 1, 10240), 'a' * 64, True)
                         for task in value['taskIds']}
            first = make_binding('a' * 40, {'adapter.py': source}, value, inventory)
            source.write_text('second', encoding='utf-8')
            changed_adapter = make_binding('a' * 40, {'adapter.py': source}, value, inventory)
            self.assertNotEqual(first.adapter_sha256, changed_adapter.adapter_sha256)
            changed_config = copy.deepcopy(value)
            changed_config['name'] = 'Another batch'
            changed = make_binding('a' * 40, {'adapter.py': source}, changed_config, inventory)
            self.assertNotEqual(first.config_sha256, changed.config_sha256)
            inventory[value['taskIds'][0]] = TaskResources(Resources(8192, 1, 10240), 'b' * 64, True)
            changed = make_binding('a' * 40, {'adapter.py': source}, value, inventory)
            self.assertNotEqual(first.task_set_sha256, changed.task_set_sha256)
            inventory.pop(value['taskIds'][0])
            with self.assertRaisesRegex(ValueError, 'inventory'):
                make_binding('a' * 40, {'adapter.py': source}, value, inventory)

    def test_task_resources_come_from_pinned_toml(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            task = root / 'tasks' / 'regex-log'
            task.mkdir(parents=True)
            (task / 'task.toml').write_text('[environment]\ncpus = 2\nmemory_mb = 8192\nstorage_mb = 10240\n', encoding='utf-8')
            with patch('workspace_plan._git', side_effect=[plan()['dataset']['commit'], '']), patch(
                    'workspace_plan._git_blob', return_value=(task / 'task.toml').read_bytes()):
                found = load_task_resources(root, ['regex-log'])
            self.assertEqual(found['regex-log'].resources, Resources(8192, 2, 10240))
            self.assertEqual(len(found['regex-log'].source_sha256), 64)
            with patch('workspace_plan._git', return_value='e' * 40):
                with self.assertRaisesRegex(ValueError, 'dataset commit'):
                    load_task_resources(root, ['regex-log'])

    def test_dirty_metadata_unknown_resource_and_path_escape_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            task = root / 'tasks' / 'regex-log'
            task.mkdir(parents=True)
            target = task / 'task.toml'
            target.write_text('[environment]\ncpus=1\nmemory_mb=1\nstorage_mb=1\ngpus=1\n', encoding='utf-8')
            with patch('workspace_plan._git', side_effect=[plan()['dataset']['commit'], '']), patch(
                    'workspace_plan._git_blob', return_value=target.read_bytes()):
                with self.assertRaisesRegex(ValueError, 'GPU'):
                    load_task_resources(root, ['regex-log'])
            with self.assertRaises(ValueError):
                load_task_resources(root, ['../outside'])

    def test_reward_zero_is_legitimate_but_missing_verification_or_cleanup_blocks(self):
        batch = results()
        self.assertEqual(evaluate_pilot(plan(), binding(), batch)['status'], 'readyForReview')
        for field, value in [('verifierCompleted', False), ('reward', None),
                             ('infrastructureError', 'provider timeout'),
                             ('severeIssue', 'duplicate execution'),
                             ('cleanupState', 'unknown'), ('resourceDebit', 1)]:
            broken = copy.deepcopy(batch)
            broken[0][field] = value
            self.assertEqual(evaluate_pilot(plan(), binding(), broken)['status'], 'blocked', field)
        self.assertEqual(evaluate_pilot(plan(), binding(), batch[:-1])['status'], 'blocked')
        self.assertEqual(evaluate_pilot(plan(), binding(), batch + batch[:1])['status'], 'blocked')

    def test_full_target_requires_its_own_review_and_pilot_report_cannot_be_tampered(self):
        report = evaluate_pilot(plan(), binding(), results())
        full = CandidateBinding('a' * 40, 'b' * 64, 'e' * 64, 'f' * 64)
        approval = dict(schema='centaeris.workspaceFullReview.v1', decision='approveFull',
                        reviewer='maintainer', candidate=full.to_dict(),
                        pilotEvidenceSha256=report['evidenceSha256'])
        require_full_review(full, report, approval)
        with self.assertRaises(ValueError):
            require_full_review(binding(), report, approval)
        report['results'][0]['cleanupState'] = 'unknown'
        with self.assertRaisesRegex(ValueError, 'digest'):
            require_full_review(full, report, approval)

    def test_metadata_dirty_or_different_from_pinned_blob_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            task = root / 'tasks' / 'regex-log'
            task.mkdir(parents=True)
            (task / 'task.toml').write_text('[environment]\ncpus=1\nmemory_mb=1\nstorage_mb=1\n', encoding='utf-8')
            with patch('workspace_plan._git', side_effect=[plan()['dataset']['commit'], ' M task.toml']):
                with self.assertRaisesRegex(ValueError, 'local modifications'):
                    load_task_resources(root, ['regex-log'])
            with patch('workspace_plan._git', side_effect=[plan()['dataset']['commit'], '']), patch(
                    'workspace_plan._git_blob', return_value=b'different'):
                with self.assertRaisesRegex(ValueError, 'pinned source'):
                    load_task_resources(root, ['regex-log'])

    def test_review_binds_candidate_taskset_config_and_pilot_evidence(self):
        batch = results()
        candidate = binding()
        report = evaluate_pilot(plan(), candidate, batch)
        approval = dict(schema='centaeris.workspaceFullReview.v1', decision='approveFull', reviewer='maintainer',
                        candidate=candidate.to_dict(), pilotEvidenceSha256=report['evidenceSha256'])
        require_full_review(candidate, report, approval)
        for field in ('productCommit', 'adapterSha256', 'configSha256', 'taskSetSha256'):
            changed = copy.deepcopy(approval)
            changed['candidate'][field] = 'f' * len(changed['candidate'][field])
            with self.assertRaises(ValueError):
                require_full_review(candidate, report, changed)
        changed = copy.deepcopy(approval)
        changed['pilotEvidenceSha256'] = 'f' * 64
        with self.assertRaises(ValueError):
            require_full_review(candidate, report, changed)
        changed = copy.deepcopy(report)
        changed['status'] = 'blocked'
        with self.assertRaises(ValueError):
            require_full_review(candidate, changed, approval)


class AdmissionTests(unittest.IsolatedAsyncioTestCase):
    async def test_nonblocking_admission_preserves_held_capacity_and_exclusivity(self):
        gate = Admission(Resources(12000, 8, 100000), Resources(1000, 1, 1000))
        one = await gate.try_acquire('one', Resources(2048, 1, 1000))
        one.mark_creation_started()
        self.assertIsNone(await gate.try_acquire('exclusive', Resources(8192, 1, 1000)))
        two = await gate.try_acquire('two', Resources(2048, 1, 1000))
        self.assertIsNone(await gate.try_acquire('third', Resources(2048, 1, 1000)))
        self.assertEqual(gate.active_count, 2)
        await two.release(cleanup_state='neverCreated')
        with self.assertRaises(ValueError):
            await one.release(cleanup_state='unknown')
        self.assertIsNone(await gate.try_acquire('exclusive', Resources(8192, 1, 1000)))
        await one.release(cleanup_state='absent')
        large = await gate.try_acquire('exclusive', Resources(8192, 1, 1000))
        self.assertIsNone(await gate.try_acquire('ordinary', Resources(2048, 1, 1000)))
        await large.release(cleanup_state='neverCreated')

    async def test_active_storage_tracks_lifecycle_and_retains_unknown_cleanup_reservation(self):
        gate = Admission(Resources(12000, 8, 20000), Resources(2000, 2, 1000))
        self.assertEqual(gate.active_storage_mib, 0)
        one = await gate.acquire('one', Resources(2048, 1, 3000))
        two = await gate.acquire('two', Resources(2048, 1, 5000))
        one.mark_creation_started()
        self.assertEqual(gate.active_storage_mib, 8000)
        with self.assertRaises(ValueError):
            await one.release(cleanup_state='unknown')
        self.assertEqual(gate.active_storage_mib, 8000)
        await two.release(cleanup_state='neverCreated')
        self.assertEqual(gate.active_storage_mib, 3000)
        await one.release(cleanup_state='absent')
        self.assertEqual(gate.active_storage_mib, 0)

    async def test_eight_gib_waits_for_drain_then_owns_full_lifecycle(self):
        gate = Admission(Resources(14336, 8, 100000), Resources(2048, 2, 1000))
        first = await gate.acquire('one', Resources(2048, 1, 1000))
        second = await gate.acquire('two', Resources(2048, 1, 1000))
        large_wait = asyncio.create_task(gate.acquire('large', Resources(8192, 2, 1000)))
        later_wait = asyncio.create_task(gate.acquire('later', Resources(2048, 1, 1000)))
        await asyncio.sleep(0)
        await first.release(cleanup_state='absent')
        await asyncio.sleep(0)
        self.assertFalse(large_wait.done())
        await second.release(cleanup_state='absent')
        large = await asyncio.wait_for(large_wait, 1)
        for phase in ('setup', 'agent', 'verifier', 'artifact', 'cleanup'):
            large.phase = phase
            await asyncio.sleep(0)
            self.assertFalse(later_wait.done(), phase)
        with self.assertRaisesRegex(ValueError, 'absence'):
            await large.release(cleanup_state='unknown')
        self.assertFalse(later_wait.done())
        await large.release(cleanup_state='absent')
        later = await asyncio.wait_for(later_wait, 1)
        await later.release(cleanup_state='absent')
        self.assertEqual(gate.active_count, 0)

    async def test_fifo_cancellation_unblocks_without_deadlock_or_overallocation(self):
        gate = Admission(Resources(12000, 8, 10000), Resources(2000, 2, 1000))
        one = await gate.acquire('one', Resources(6000, 2, 1000))
        cancelled = asyncio.create_task(gate.acquire('big', Resources(8192, 2, 1000)))
        ordinary = asyncio.create_task(gate.acquire('two', Resources(3000, 2, 1000)))
        await asyncio.sleep(0)
        self.assertFalse(ordinary.done())
        cancelled.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await cancelled
        two = await asyncio.wait_for(ordinary, 1)
        third_wait = asyncio.create_task(gate.acquire('three', Resources(1, 1, 1)))
        await asyncio.sleep(0)
        self.assertFalse(third_wait.done())
        await two.release(cleanup_state='neverCreated')
        three = await asyncio.wait_for(third_wait, 1)
        await three.release(cleanup_state='absent')
        await one.release(cleanup_state='absent')

    async def test_uncertain_creation_never_releases_as_never_created(self):
        gate = Admission(Resources(12000, 8, 10000), Resources(2000, 2, 1000))
        allocation = await gate.acquire('one', Resources(8192, 1, 1000))
        allocation.mark_creation_started()
        with self.assertRaisesRegex(ValueError, 'absence'):
            await allocation.release(cleanup_state='neverCreated')
        self.assertEqual(gate.active_count, 1)
        await allocation.release(cleanup_state='absent')

    async def test_resource_reserve_oversize_and_duplicate_identity(self):
        gate = Admission(Resources(10000, 8, 10000), Resources(2000, 2, 1000))
        for request in (Resources(8192, 1, 1), Resources(1, 7, 1), Resources(1, 1, 9001)):
            with self.assertRaisesRegex(ValueError, 'budget'):
                await gate.acquire('large', request)
        one = await gate.acquire('one', Resources(4000, 2, 1000))
        with self.assertRaisesRegex(ValueError, 'identity'):
            await gate.acquire('one', Resources(1, 1, 1))
        await one.release(cleanup_state='absent')
        with self.assertRaisesRegex(ValueError, 'released'):
            await one.release(cleanup_state='absent')


if __name__ == '__main__':
    unittest.main()
