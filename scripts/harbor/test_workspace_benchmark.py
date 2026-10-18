"""Pinned-source, native Harbor configuration and pilot admission behavior."""
import asyncio
import copy
import json
import io
from pathlib import Path
import subprocess
import tempfile
import tarfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from workspace_benchmark import freeze_tasks, task_profile, native_trial_config, BenchmarkRunner, DockerImagePreparer, _REGISTRY, _acquire_or_halt, _TrialProjection, RegisteredWorkspaceEnvironment
from workspace_plan import Admission, Resources, CandidateBinding, canonical_digest, evaluate_pilot
from harbor.trial.trial import Trial
from harbor.models.task.task import Task


class BenchmarkTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.dataset = self.root / "dataset"
        self.dataset.mkdir()
        self.git("init", "-q")
        self.git("config", "user.name", "Fixture")
        self.git("config", "user.email", "fixture@example.test")
        self.git("config", "core.autocrlf", "false")
        task = self.dataset / "tasks" / "task-one"
        (task / "environment").mkdir(parents=True)
        (task / "tests").mkdir()
        (task / "instruction.md").write_text("<!-- test canary -->\n\nOriginal instruction\n")
        (task / "environment" / "Dockerfile").write_text("FROM scratch\n")
        (task / "tests" / "test.sh").write_text("#!/bin/bash\nexit 0\n")
        (task / "task.toml").write_text('[environment]\ncpus=1\nmemory_mb=2048\nstorage_mb=10240\n[agent]\ntimeout_sec=12000\n')
        self.git("add", ".")
        self.git("commit", "-qm", "Create fixture")
        self.commit = self.git("rev-parse", "HEAD")

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.dataset), *args], text=True, stderr=subprocess.PIPE).strip()

    def test_freeze_covers_instruction_dockerfile_verifier_and_rejects_dirty_bytes(self):
        with patch("workspace_benchmark.DATASET_COMMIT", self.commit):
            frozen = freeze_tasks(self.dataset, ["task-one"])
            self.assertIn("environment/Dockerfile", frozen["task-one"]["files"])
            self.assertIn("tests/test.sh", frozen["task-one"]["files"])
            (self.dataset / "tasks/task-one/tests/test.sh").write_text("tampered\n")
            with self.assertRaises(ValueError):
                freeze_tasks(self.dataset, ["task-one"])

    def test_ignored_extra_files_and_untracked_files_are_rejected(self):
        with patch("workspace_benchmark.DATASET_COMMIT", self.commit):
            (self.dataset / "tasks/task-one/extra").write_text("extra")
            with self.assertRaises(ValueError):
                freeze_tasks(self.dataset, ["task-one"])

    def test_native_harbor_config_contains_only_opaque_binding_not_credentials(self):
        config = native_trial_config(self.dataset / "tasks/task-one", self.root / "job", "trial-one", "binding-opaque", "model-ref", {"id": "example/alternate", "thinking": "low"})
        wire = json.loads(config.model_dump_json())
        self.assertEqual(wire["environment"]["kwargs"], {"controllerBinding": "binding-opaque"})
        self.assertEqual(wire["timeout_multiplier"], 1)
        self.assertFalse(wire["verifier"]["disable"])
        self.assertEqual(wire["agent"]["kwargs"]["thinking_mode"], "low")
        self.assertEqual(wire["agent"]["model_name"], "example/alternate")
        self.assertEqual(wire["extra_instruction_paths"], [])

    def test_profile_uses_exact_task_resources_image_user_and_workdir(self):
        from harbor.models.task.task import Task
        task = Task(self.dataset / "tasks/task-one")
        profile = task_profile(task, {"imageDigest": "sha256:" + "a" * 64, "workspaceRoot": "/task", "user": "1000:1000"})
        self.assertEqual(profile["workspaceRoot"], "/task")
        self.assertEqual(profile["user"], "1000:1000")
        self.assertEqual(profile["resources"]["memoryBytes"], 2048 * 1024**2)
        self.assertEqual(profile["resources"]["storageBytes"], 10240 * 1024**2)
        self.assertEqual(profile["resources"]["pidsLimit"], 4096)


class RunnerTests(unittest.IsolatedAsyncioTestCase):
    async def test_direct_full_requires_exact_approval_and_runs_eighty_nine_tasks_once(self):
        from unittest.mock import AsyncMock
        with tempfile.TemporaryDirectory() as temporary:
            registry = json.loads(Path(__file__).with_name('full-tasks.json').read_text())
            tasks = [row['path'].removeprefix('tasks/') for row in registry['tasks']]
            runner = object.__new__(BenchmarkRunner)
            runner.plan = {'taskIds': tasks, 'attempts': 1}
            runner.source_digest, runner.candidate_digest = 'source', 'candidate'
            runner._run_batch = AsyncMock(return_value={'batchDir': temporary, 'results': [], 'status': 'finished'})
            approval = {'schema': 'centaeris.workspaceBenchmarkFullApproval.v1', 'authorized': True,
                        'admission': 'directFull', 'candidateDigest': 'candidate', 'sourceDigest': 'source',
                        'taskIds': tasks, 'attempts': 1}
            for change in ({'authorized': False}, {'candidateDigest': 'other'}, {'sourceDigest': 'other'},
                           {'taskIds': tasks[:-1]}, {'taskIds': list(reversed(tasks))}, {'attempts': 5},
                           {'attempts': True}, {'unexpected': True}):
                with self.subTest(change=change), self.assertRaises(ValueError):
                    await runner.run_full(approval={**approval, **change})
            runner._run_batch.assert_not_called()
            report = await runner.run_full(approval=approval)
            runner._run_batch.assert_awaited_once_with('full', tasks, attempts=1)
            self.assertEqual(report['admission'], approval)
            self.assertEqual(json.loads((Path(temporary) / 'batch-result.json').read_text())['admission'], approval)
            runner.plan['attempts'] = 5
            with self.assertRaises(ValueError):
                await runner.run_full(approval=approval)

    async def _run_failure_batch(self, *, cleanup_pending=False, task_count=20, stage='pilot', disk_free=None, after_pull_free=None, invalid_verifier=False):
        with tempfile.TemporaryDirectory() as temporary:
            task_ids = [f"task-{index}" for index in range(task_count)]
            for task_id in task_ids:
                tests = Path(temporary) / 'tasks' / task_id / 'tests'
                tests.mkdir(parents=True)
                (tests / 'test.sh').write_text('exit 0\n')
            if invalid_verifier:
                with (tests / 'private_test.tar.gz').open('wb') as output:
                    output.truncate(32 * 1024 * 1024 + 1)
            admission = Admission(Resources(12000, 8, 100000), Resources(1000, 1, 1000))
            first_started, second_started = asyncio.Event(), asyncio.Event()
            started, active_counts, started_trial_ids = [], [], []
            runner = object.__new__(BenchmarkRunner)
            runner.plan = {"taskIds": task_ids, "model": {"id": "fixture", "thinking": "low"}}
            runner.dataset_root, runner.output_root = Path(temporary), Path(temporary) / "jobs"
            runner.sources = {task_id: {} for task_id in task_ids}
            runner.source_digest, runner.candidate_digest = "source", "candidate"
            runner.candidate = SimpleNamespace(to_dict=lambda: {"id": "candidate"})
            runner.admission, runner.client_factory, runner.provisioner = admission, lambda: object(), object()
            runner.workspace_id, runner.agent_id, runner.model_ref = "workspace", "agent", "model"
            image_calls = []
            async def prepare(task, logs):
                image_calls.append(task.paths.task_dir.name)
                await asyncio.sleep(0)
                if after_pull_free is not None:
                    disk_free[0] = after_pull_free
                return {}
            async def gate():
                return True
            runner.image_preparer = SimpleNamespace(prepare=prepare, snapshot_locks=lambda: {'tasks': {}})
            if disk_free is not None:
                from workspace_launch import CapacityCheckedImages
                runner.image_preparer = CapacityCheckedImages(runner.image_preparer, Path(temporary),
                    8192, lambda: admission.active_storage_mib)
            runner.physical_gate = gate
            runner.sink_factory = lambda *args: SimpleNamespace(submit=lambda *args, **kwargs: True, close=lambda: None)
            runner.batch_started = None
            def task(path):
                memory = 8192 if cleanup_pending and path.name == "task-2" else 2048
                return SimpleNamespace(config=SimpleNamespace(environment=SimpleNamespace(
                    memory_mb=memory, cpus=1, storage_mb=10240 if disk_free is not None else 1000)),
                    paths=SimpleNamespace(environment_dir=path / "environment", task_dir=path))
            async def create(config):
                entry = _REGISTRY[config.binding]
                task_id = config.task_id
                allocation = entry["ports"]["allocation"]
                class Environment:
                    async def stop(self, delete=True):
                        if cleanup_pending and task_id == "task-0":
                            raise ValueError("physical state unknown")
                        if not allocation.released:
                            await allocation.release(cleanup_state="absent")
                entry["environment"] = Environment()
                class FakeTrial:
                    def add_hook(self, *args):
                        pass
                    async def run(self):
                        allocation.mark_creation_started()
                        started.append(task_id)
                        started_trial_ids.append(config.trial_id)
                        active_counts.append(admission.active_count)
                        if task_id == "task-0" and disk_free is None:
                            first_started.set()
                            await second_started.wait()
                            raise ValueError("observed infrastructure error")
                        if task_id == "task-1" and disk_free is None:
                            second_started.set()
                            await first_started.wait()
                        await asyncio.sleep(0)
                        return SimpleNamespace(verifier_result=SimpleNamespace(rewards={"reward": 1}), exception_info=None)
                return FakeTrial()
            def config(path, root, trial_id, binding, model_ref, model):
                return SimpleNamespace(binding=binding, task_id=path.name, trial_id=trial_id)
            try:
                with patch("workspace_benchmark.freeze_tasks", side_effect=lambda root, ids: {key: {} for key in ids}), \
                        patch("workspace_benchmark.platform.system", return_value="Linux"), \
                        patch("workspace_benchmark.Task", side_effect=task), \
                        patch("workspace_benchmark.task_profile", return_value={}), \
                        patch("workspace_benchmark.native_trial_config", side_effect=config), \
                        patch("workspace_benchmark.Trial.create", side_effect=create), \
                        patch("workspace_launch.shutil.disk_usage", side_effect=lambda path: SimpleNamespace(free=disk_free[0] * 1024**2) if disk_free is not None else None):
                    if invalid_verifier:
                        from workspace_client import WorkspaceError
                        with self.assertRaisesRegex(WorkspaceError, 'transfer_expanded_limit_exceeded'):
                            await runner._run_batch(stage, task_ids)
                        self.assertEqual(started, [])
                        self.assertEqual(image_calls, [])
                        self.assertEqual(admission.active_count, 0)
                        return
                    report = await asyncio.wait_for(runner._run_batch(stage, task_ids), 3)
                if disk_free is not None:
                    report['preparedTaskIds'] = image_calls
                    observations = Path(report['batchDir']) / 'capacity-observations.jsonl'
                    report['capacityObservations'] = [json.loads(line) for line in observations.read_text().splitlines()] if observations.exists() else []
                    report['startedTrialIds'] = started_trial_ids
                return report, started, active_counts, admission.active_count
            finally:
                for binding, entry in list(_REGISTRY.items()):
                    if entry["ports"]["provisioner"] is runner.provisioner:
                        _REGISTRY.pop(binding)
                        allocation = entry["ports"]["allocation"]
                        if not allocation.released:
                            await allocation.release(cleanup_state="absent")

    async def test_all_selected_verifier_inputs_are_checked_before_the_first_agent(self):
        await self._run_failure_batch(invalid_verifier=True)

    async def test_live_disk_shortfall_reduces_concurrency_without_consuming_attempts(self):
        report, started, counts, held = await self._run_failure_batch(disk_free=[24576])
        self.assertEqual(report['status'], 'finished')
        self.assertEqual(set(started), {f'task-{i}' for i in range(20)})
        self.assertEqual(len(started), 20)
        self.assertEqual(max(counts), 1)
        self.assertEqual(len(report['results']), 20)
        self.assertEqual(report['unrun'], [])
        self.assertEqual(held, 0)

    async def test_image_pull_capacity_drop_defers_same_attempt_without_repull_or_spawn(self):
        report, started, counts, held = await self._run_failure_batch(
            disk_free=[40960], after_pull_free=24576)
        self.assertEqual(report['status'], 'finished')
        self.assertEqual(len(started), 20)
        self.assertEqual(len(set(started)), 20)
        self.assertEqual(sorted(report['preparedTaskIds']), sorted(started))
        self.assertEqual({row['attempt'] for row in report['results']}, {1})
        self.assertEqual(report['unrun'], [])
        self.assertTrue(report['capacityObservations'])
        self.assertTrue(all(row['trialId'] in report['startedTrialIds'] for row in report['capacityObservations']))
        self.assertTrue(all(row['freeBytes'] == 24576 * 1024**2 and row['requiredBytes'] == 28672 * 1024**2
                            for row in report['capacityObservations']))
        self.assertEqual(held, 0)

    async def test_post_pull_shortfall_without_enough_for_one_trial_preserves_unrun_attempts(self):
        report, started, _, held = await self._run_failure_batch(disk_free=[40960], after_pull_free=16384)
        self.assertEqual(report['status'], 'blocked')
        self.assertEqual(report['results'], [])
        self.assertEqual(started, [])
        self.assertEqual(len(report['unrun']), 20)
        self.assertLessEqual(len(report['preparedTaskIds']), 2)
        self.assertEqual(len(set(report['preparedTaskIds'])), len(report['preparedTaskIds']))
        self.assertTrue(report['capacityObservations'])
        self.assertTrue(all(row['reason'] == 'diskCapacityUnavailable' for row in report['unrun']))
        self.assertEqual(held, 0)

    async def test_disk_shortfall_with_no_peer_blocks_without_draining_or_preparing_tasks(self):
        report, started, _, held = await self._run_failure_batch(disk_free=[16384])
        self.assertEqual(report['status'], 'blocked')
        self.assertEqual(started, [])
        self.assertEqual(report['results'], [])
        self.assertEqual(report['preparedTaskIds'], [])
        self.assertEqual(len(report['unrun']), 20)
        self.assertTrue(all(row['reason'] == 'diskCapacityUnavailable' for row in report['unrun']))
        self.assertEqual(report['unrun'][0]['freeBytes'], 16384 * 1024**2)
        self.assertEqual(report['unrun'][0]['requiredBytes'], 18432 * 1024**2)
        self.assertEqual(held, 0)

    async def test_trial_infrastructure_failure_does_not_block_remaining_twenty_task_batch(self):
        report, started, active_counts, held = await self._run_failure_batch()
        self.assertEqual(set(started), {f"task-{index}" for index in range(20)})
        self.assertEqual(len(report["results"]), 20)
        self.assertEqual(report["status"], "blocked")
        self.assertEqual(sum(row["infrastructureError"] is not None for row in report["results"]), 1)
        self.assertEqual(held, 0)
        self.assertLessEqual(max(active_counts), 2)

    async def test_single_attempt_full_batch_runs_each_task_once(self):
        report, started, active_counts, held = await self._run_failure_batch(task_count=89, stage='full')
        self.assertEqual(len(started), 89)
        self.assertEqual(len(set(started)), 89)
        self.assertEqual(len(report['results']), 89)
        self.assertEqual({row['attempt'] for row in report['results']}, {1})
        self.assertEqual(report['stage'], 'full')
        self.assertEqual(report['attemptsPerTask'], 1)
        self.assertEqual(held, 0)
        self.assertLessEqual(max(active_counts), 2)

    async def test_pending_cleanup_keeps_capacity_but_other_tasks_continue_without_deadlock(self):
        report, started, active_counts, held = await self._run_failure_batch(cleanup_pending=True)
        self.assertEqual(set(started), {f"task-{index}" for index in range(20)} - {"task-2"})
        self.assertEqual(held, 1)
        self.assertLessEqual(max(active_counts), 2)
        self.assertEqual(report["status"], "blocked")
        self.assertEqual(report["unrun"], [{"taskId": "task-2", "attempt": 1, "reason": "retainedCapacityUnavailable"}])
        failed = next(row for row in report["results"] if row["taskId"] == "task-0")
        self.assertEqual((failed["cleanupState"], failed["resourceDebit"]), ("pending", 1))

    async def test_direct_pilot_requires_explicit_approval_of_exact_candidate_and_twenty_tasks(self):
        from unittest.mock import AsyncMock
        with tempfile.TemporaryDirectory() as temporary:
            runner = object.__new__(BenchmarkRunner)
            tasks = [f'task-{index}' for index in range(20)]
            runner.plan = {'taskIds': tasks, 'attempts': 1}
            runner.source_digest, runner.candidate_digest = 'source', 'candidate'
            runner.candidate = object()
            runner.image_preparer = SimpleNamespace(snapshot_locks=lambda: {'tasks': {name: {} for name in tasks}})
            runner._run_batch = AsyncMock(return_value={'batchDir': temporary, 'results': [], 'status': 'finished'})
            approval = {'schema': 'centaeris.workspaceBenchmarkPilotApproval.v1', 'authorized': True,
                        'admission': 'directPilot', 'candidateDigest': 'candidate', 'sourceDigest': 'source', 'taskIds': tasks}
            for change in ({'authorized': False}, {'candidateDigest': 'other'}, {'sourceDigest': 'other'},
                           {'taskIds': tasks[:-1]}, {'unexpected': True}):
                with self.subTest(change=change), self.assertRaises(ValueError):
                    await runner.run_pilot(approval={**approval, **change})
                runner._run_batch.assert_not_awaited()
            with self.assertRaises(ValueError):
                await runner.run_pilot({'status': 'failed'}, approval=approval)
            with patch('workspace_benchmark.evaluate_pilot', return_value={'status': 'passed', 'issues': []}):
                report = await runner.run_pilot(approval=approval)
            runner._run_batch.assert_awaited_once_with('pilot', tasks)
            self.assertEqual(report['admission'], approval)
            self.assertEqual(report['review']['imageLocks'].keys(), set(tasks))

    async def test_image_optional_user_and_workdir_use_docker_defaults(self):
        fixture = BenchmarkTests(methodName="test_profile_uses_exact_task_resources_image_user_and_workdir")
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        task = Task(fixture.dataset / "tasks/task-one")
        task.config.environment.docker_image = "registry.example.test/task:original"
        image = "sha256:" + "a" * 64
        repository = "registry.example.test/task@sha256:" + "b" * 64
        async def capture(endpoint, args, **kwargs):
            if args[-2] == "{{.Id}}":
                return image.encode()
            return json.dumps([image, None, None, "linux", "amd64", [repository]]).encode()
        preparer = DockerImagePreparer("unix:///test/docker.sock", fixture.root / "image-locks.json")
        with patch("workspace_benchmark._docker_capture", capture):
            result = await preparer.prepare(task, fixture.root / "preflight")
        self.assertEqual(result["workspaceRoot"], "/")
        self.assertEqual(result["user"], "0:0")
        self.assertEqual(result["originalImageUser"], "")
        self.assertEqual(result["imageDigest"], image)
        self.assertEqual(result["repoDigests"], [repository])

    async def test_image_explicit_user_and_workdir_are_preserved(self):
        image = "sha256:" + "a" * 64
        async def capture(endpoint, args, **kwargs):
            return json.dumps([image, "/original", "1000:1001", "linux", "amd64", []]).encode()
        preparer = DockerImagePreparer("unix:///test/docker.sock")
        with patch("workspace_benchmark._docker_capture", capture):
            result = await preparer._inspect_image(image)
        self.assertEqual(result[1:3], ["/original", "1000:1001"])
        self.assertEqual(await preparer._user(image, result[2], None), "1000:1001")

    async def test_image_defaults_do_not_accept_invalid_identity_platform_or_field_types(self):
        image = "sha256:" + "a" * 64
        for index, invalid in ((0, "mutable-tag"), (1, 0), (1, []), (2, False), (2, 1000),
                               (3, "windows"), (4, "arm64"), (5, ["mutable-tag"])):
            with self.subTest(index=index, invalid=invalid):
                metadata = [image, None, None, "linux", "amd64", []]
                metadata[index] = invalid
                async def capture(endpoint, args, **kwargs):
                    return json.dumps(metadata).encode()
                with patch("workspace_benchmark._docker_capture", capture), self.assertRaises(ValueError):
                    await DockerImagePreparer("unix:///test/docker.sock")._inspect_image(image)

    async def test_optional_metrics_merge_current_phase_and_ignore_late_callbacks(self):
        events, factories, samplers = [], [], []
        class Sink:
            def submit(self, kind, payload, **kwargs):
                events.append(copy.deepcopy(payload))
        class Sampler:
            def start(self):
                self.started = True
            def close(self):
                self.closed = True
        def factory(session, run, container, callback):
            factories.append((session, run, container, callback))
            sampler = Sampler()
            samplers.append(sampler)
            return sampler
        owned = SimpleNamespace(session_id="session", agent_run_id="run", instance_id="instance", container_id="a" * 64)
        environment = SimpleNamespace(_owned=owned, _destroyed=False)
        projection = _TrialProjection(Sink(), "trial", {"phase": "preparing", "status": "running", "resourceHolding": 1}, factory)
        registered = object.__new__(RegisteredWorkspaceEnvironment)
        registered._controller_entry = {"metricsProjection": projection}
        registered._owned, registered._destroyed = owned, False
        async def start(*args, **kwargs):
            pass
        with patch("workspace_benchmark.WorkspaceEnvironment.start", start):
            await registered.start()
            await registered.start()
        self.assertEqual(len(factories), 1)
        self.assertEqual(factories[0][:2], ("session", "run"))
        self.assertTrue(samplers[0].started)
        supplier, callback = factories[0][2:]
        self.assertEqual(supplier(), "a" * 64)
        projection.project({"phase": "verifier"})
        await asyncio.to_thread(callback, {"observedAt": "2026-10-17T00:00:00Z", "memory": {"taskCurrentBytes": 12}})
        self.assertEqual(events[-1]["phase"], "verifier")
        self.assertEqual(events[-1]["metrics"]["memory"]["taskCurrentBytes"], 12)
        registered._owned = SimpleNamespace(**{**vars(owned), "container_id": "b" * 64})
        self.assertIsNone(supplier())
        projection.close()
        self.assertTrue(samplers[0].closed)
        projection.project({"phase": "finished", "status": "completed", "resourceHolding": 0})
        count = len(events)
        callback({"memory": {"taskCurrentBytes": 99}})
        self.assertEqual(len(events), count)
        self.assertEqual(events[-1]["status"], "completed")
        self.assertIsNone(events[-1]["metrics"]["memory"]["taskCurrentBytes"])

    async def test_optional_sampler_failures_leave_lifecycle_events_and_verification_intact(self):
        events = []
        class Sink:
            def submit(self, kind, payload, **kwargs):
                events.append(copy.deepcopy(payload))
        def broken(*args):
            raise RuntimeError("optional telemetry unavailable")
        projection = _TrialProjection(Sink(), "trial", {"phase": "preparing"}, broken)
        projection.start(SimpleNamespace(_owned=SimpleNamespace(session_id="s", agent_run_id="r", instance_id="i", container_id="a" * 64), _destroyed=False))
        projection.project({"phase": "verifier", "reward": 1})
        projection.close()
        projection.project({"phase": "finished", "status": "completed", "reward": 1})
        self.assertEqual(events[-1], {"phase": "finished", "status": "completed", "reward": 1})

    async def test_mutable_image_tag_is_resolved_once_and_locked_across_preflight_and_pilot(self):
        fixture = BenchmarkTests(methodName="test_profile_uses_exact_task_resources_image_user_and_workdir")
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        task = Task(fixture.dataset / "tasks/task-one")
        task.config.environment.docker_image = "registry.example.test/task:mutable"
        first, changed = "sha256:" + "a" * 64, "sha256:" + "b" * 64
        repository = "registry.example.test/task@sha256:" + "c" * 64
        tag_value, calls = [first], []
        async def capture(endpoint, args, **kwargs):
            calls.append(args)
            requested = args[-1]
            image = tag_value[0] if requested == task.config.environment.docker_image else requested
            if args[-2] == "{{.Id}}":
                return image.encode()
            return json.dumps([image, "/original", "", "linux", "amd64", [repository]]).encode()
        path = fixture.root / "image-locks.json"
        preparer = DockerImagePreparer("unix:///test/docker.sock", path)
        with patch("workspace_benchmark._docker_capture", capture):
            original = await preparer.prepare(task, fixture.root / "preflight")
            tag_value[0] = changed
            before = len(calls)
            reloaded = DockerImagePreparer("unix:///test/docker.sock", path)
            again = await reloaded.prepare(task, fixture.root / "pilot")
        self.assertEqual(original["imageDigest"], first)
        self.assertEqual(again, original)
        self.assertTrue(all(args[-1] == first for args in calls[before:]))
        self.assertEqual(json.loads(path.read_text())["tasks"][task.short_name]["repoDigests"], [repository])

    async def test_missing_locked_image_uses_repository_digest_and_never_mutable_tag(self):
        fixture = BenchmarkTests(methodName="test_profile_uses_exact_task_resources_image_user_and_workdir")
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        task = Task(fixture.dataset / "tasks/task-one")
        image, repository = "sha256:" + "a" * 64, "registry.example.test/task@sha256:" + "c" * 64
        path = fixture.root / "image-locks.json"
        path.write_text(json.dumps({"schema": "centaeris.workspaceBenchmarkImages.v1", "tasks": {
            task.short_name: {"sourceChecksum": task.checksum, "imageDigest": image, "workspaceRoot": "/app",
                "user": "0:0", "originalImageUser": "", "imageReference": "registry.example.test/task:mutable", "repoDigests": [repository]}}}))
        preparer = DockerImagePreparer("unix:///test/docker.sock", path)
        pulls, observations = [], [False]
        async def inspect(request):
            self.assertEqual(request, image)
            if not observations[0]:
                raise ValueError("local image missing")
            return [image, "/app", "", "linux", "amd64", [repository]]
        async def pull(request, *args):
            pulls.append(request)
            observations[0] = True
        with patch.object(preparer, "_inspect_image", inspect), patch.object(preparer, "_pull", pull):
            result = await preparer.prepare(task, fixture.root / "full")
        self.assertEqual(pulls, [repository])
        self.assertEqual(result["imageDigest"], image)

    async def test_image_user_resolution_reads_stopped_image_metadata_without_changing_the_image(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            calls, image, container = [], "sha256:" + "a" * 64, "b" * 64
            body = b"root:x:0:0:root:/root:/bin/bash\nworker:x:1000:1001::/home/worker:/bin/bash\n"
            stream = io.BytesIO()
            with tarfile.open(fileobj=stream, mode="w") as archive:
                info = tarfile.TarInfo("passwd")
                info.size = len(body)
                archive.addfile(info, io.BytesIO(body))
            async def capture(endpoint, args, **kwargs):
                calls.append(args)
                if args[0] == "create":
                    return container.encode()
                if args[0] == "cp":
                    return stream.getvalue()
                if args[0] == "rm":
                    return container.encode()
                raise AssertionError(args)
            preparer = DockerImagePreparer("unix:///test/docker.sock")
            with patch("workspace_benchmark._docker_capture", capture):
                self.assertEqual(await preparer._user(image, "worker", root), "1000:1001")
            self.assertEqual([args[0] for args in calls], ["create", "cp", "rm"])
            evidence = json.loads(next(root.glob("benchmark-metadata-*.json")).read_text())
            self.assertEqual(evidence["state"], "absent")
            self.assertEqual(calls[-1], ["rm", "-v", container])

    async def test_lost_metadata_create_ack_reconciles_only_its_labelled_instance(self):
        with tempfile.TemporaryDirectory() as temporary:
            calls, image, container = [], "sha256:" + "a" * 64, "b" * 64
            async def capture(endpoint, args, **kwargs):
                calls.append(args)
                if args[0] == "create":
                    raise ValueError("lost create ACK")
                if args[0] == "inspect":
                    return json.dumps([container, image, args[-1]]).encode()
                if args[0] == "rm":
                    return container.encode()
                raise AssertionError(args)
            with patch("workspace_benchmark._docker_capture", capture), self.assertRaisesRegex(ValueError, "lost create ACK"):
                await DockerImagePreparer("unix:///test/docker.sock")._identity_file(image, "/etc/passwd", temporary)
            self.assertEqual([args[0] for args in calls], ["create", "inspect", "rm"])
            self.assertEqual(json.loads(next(Path(temporary).glob("*.json")).read_text())["state"], "absent")

    async def test_preflight_holds_admission_before_image_and_checks_service_before_cleanup(self):
        fixture = BenchmarkTests(methodName="test_profile_uses_exact_task_resources_image_user_and_workdir")
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        admission = Admission(Resources(10000, 8, 100000), Resources(1000, 1, 1000))
        observations, telemetry_attempts = [], []
        class Preparer:
            async def prepare(self, task, logs):
                observations.append(("image", admission.active_count))
                return {"imageDigest": "sha256:" + "a" * 64, "workspaceRoot": "/app", "user": "0:0"}
        class Sink:
            def submit(self, *args, **kwargs):
                return True
            def close(self):
                pass
        async def gate():
            return True
        runner = object.__new__(BenchmarkRunner)
        runner.plan = {"taskIds": ["task-one"], "model": {"id": "fake", "thinking": "high"}}
        runner.dataset_root = fixture.dataset
        runner.output_root = fixture.root / "jobs"
        with patch("workspace_benchmark.DATASET_COMMIT", fixture.commit):
            runner.sources = freeze_tasks(fixture.dataset, ["task-one"])
        runner.source_digest, runner.candidate_digest = "source", "candidate"
        runner.candidate = SimpleNamespace(to_dict=lambda: {"id": "candidate"})
        runner.admission, runner.client_factory, runner.provisioner = admission, lambda: object(), object()
        runner.workspace_id, runner.agent_id, runner.model_ref = "workspace", "agent", "model-ref"
        runner.image_preparer, runner.physical_gate = Preparer(), gate
        runner.sink_factory, runner.batch_started = lambda *args: Sink(), None
        def unavailable_metrics(*args):
            telemetry_attempts.append(True)
            raise ValueError("optional metrics unavailable")
        runner.metrics_factory = unavailable_metrics
        async def create(config):
            entry = _REGISTRY[config.environment.kwargs["controllerBinding"]]
            allocation = entry["ports"]["allocation"]
            class Environment:
                alive = True
                _destroyed = False
                _owned = SimpleNamespace(session_id="session", agent_run_id="run", instance_id="instance", container_id="a" * 64)
                async def stop(self, delete=True):
                    if self.alive:
                        self.alive = False
                        observations.append(("cleanup", admission.active_count))
                        await allocation.release(cleanup_state="absent")
            environment = Environment()
            entry["environment"] = environment
            class FakeTrial:
                result = SimpleNamespace(verifier_result=SimpleNamespace(rewards={"reward": 1}), exception_info=None)
                def add_hook(self, *args):
                    pass
                async def run(self):
                    allocation.mark_creation_started()
                    entry["metricsProjection"].start(environment)
                    observations.append(("verifier", admission.active_count))
                    await entry["preDestroyCheck"](environment)
                    await environment.stop()
                    return self.result
            return FakeTrial()
        async def check(result, environment):
            self.assertTrue(environment.alive)
            observations.append(("service", admission.active_count))
            return {"actualToolEffects": True, "artifactDownload": True, "backgroundContinuity": True}
        with patch("workspace_benchmark.DATASET_COMMIT", fixture.commit), patch("workspace_benchmark.platform.system", return_value="Linux"), patch("workspace_benchmark.Trial.create", create):
            report = await runner._run_batch("preflight", ["task-one"], service_checks=check)
        self.assertEqual(observations, [("image", 1), ("verifier", 1), ("service", 1), ("cleanup", 1)])
        self.assertEqual(report["status"], "finished")
        self.assertEqual(admission.active_count, 0)
        self.assertEqual(telemetry_attempts, [True])
        self.assertEqual(report["results"][0]["reward"], 1)
        self.assertIsNone(report["results"][0]["infrastructureError"])
        self.assertTrue((Path(report["batchDir"]) / "batch-result.json").is_file())

    async def test_actual_native_trial_factory_resolves_private_ports_without_serializing_them(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            task_dir = root / "task"
            (task_dir / "environment").mkdir(parents=True)
            (task_dir / "tests").mkdir()
            (task_dir / "instruction.md").write_text("task\n")
            (task_dir / "environment/Dockerfile").write_text("FROM scratch\n")
            (task_dir / "tests/test.sh").write_text("exit 0\n")
            (task_dir / "task.toml").write_text("[environment]\ncpus=1\nmemory_mb=2048\nstorage_mb=10240\n")
            admission = Admission(Resources(10000, 8, 100000), Resources(1000, 1, 1000))
            allocation = await admission.acquire("native", Resources(2048, 1, 10240))
            profile = task_profile(Task(task_dir), {"imageDigest": "sha256:" + "a" * 64, "workspaceRoot": "/app", "user": "0:0"})
            marker = object()
            entry = {"environmentDir": (task_dir / "environment").resolve(), "trialId": "native",
                "environment": None, "ports": {"workspace_client": marker, "provisioner": marker,
                    "allocation": allocation, "workspace_id": "workspace", "agent_id": "agent", "trial_id": "native",
                    "profile": profile, "model_config_ref": "model-ref", "thinking_mode": "high"}}
            _REGISTRY["opaque-native"] = entry
            trial = None
            try:
                config = native_trial_config(task_dir, root / "job", "native", "opaque-native", "model-ref", {"id": "fixture", "thinking": "high"})
                trial = await Trial.create(config)
                self.assertIs(trial.agent_environment.workspace_client, marker)
                self.assertIs(entry["environment"], trial.agent_environment)
                self.assertTrue(trial.paths.lock_path.is_file())
                self.assertNotIn(str(marker), json.dumps(json.loads(config.model_dump_json())))
                observed = []
                async def before_destroy(environment):
                    observed.append(allocation.released)
                entry["preDestroyCheck"] = before_destroy
                await trial.agent_environment.stop(delete=True)
                self.assertEqual(observed, [False])
                self.assertTrue(allocation.released)
            finally:
                _REGISTRY.pop("opaque-native", None)
                if trial:
                    trial._close_logger_handler()

    async def test_one_preflight_requires_reward_one_but_zero_is_not_infra_failure(self):
        runner = object.__new__(BenchmarkRunner)
        runner.source_digest = "source"
        runner.candidate_digest = "candidate"
        runner.plan = {"taskIds": [f"task-{index}" for index in range(20)]}
        runner._run_batch = lambda *args, **kwargs: None
        reward = [1]
        async def batch(stage, ids, service_checks=None):
            return {"stage": stage, "taskIds": ids, "sourceDigest": "source", "candidateDigest": "candidate",
                    "results": [{"verifierCompleted": True, "reward": reward[0], "infrastructureError": None,
                                 "severeIssue": None, "cleanupState": "absent", "resourceDebit": 0}],
                    "serviceChecks": {"actualToolEffects": True, "artifactDownload": True, "backgroundContinuity": True}}
        runner._run_batch = batch
        report = await runner.run_preflight("task-0", service_checks=lambda *args: {})
        self.assertEqual(report["taskIds"], ["task-0"])
        self.assertEqual(report["stage"], "preflight")
        self.assertEqual(report["status"], "passed")
        reward[0] = 0
        model_failure = await runner.run_preflight("task-0", service_checks=lambda *args: {})
        self.assertEqual(model_failure["status"], "blocked")
        self.assertIsNone(model_failure["results"][0]["infrastructureError"])
        broken = copy.deepcopy(report)
        broken["sourceDigest"] = "other"
        with self.assertRaises(ValueError):
            await runner.run_pilot(broken)

    async def test_no_automatic_full_run_and_unproven_preflight_cannot_start_pilot(self):
        runner = object.__new__(BenchmarkRunner)
        runner.source_digest = "source"
        runner.candidate_digest = "candidate"
        runner.plan = {"taskIds": [f"task-{index}" for index in range(20)]}
        with self.assertRaises(ValueError):
            await runner.run_pilot({"status": "passed", "stage": "pilot"})

    async def test_full_batch_requires_root_review_artifact_bound_to_exact_candidate(self):
        plan = json.loads(Path(__file__).with_name("workspace-pilot-tasks.json").read_text())
        pilot_candidate = CandidateBinding("1" * 40, "2" * 64, canonical_digest(plan), "3" * 64)
        results = [{"trialId": "trial-" + str(index), "taskId": task, "attempt": 1,
            "verifierCompleted": True, "reward": 0, "infrastructureError": None, "severeIssue": None,
            "cleanupState": "absent", "resourceDebit": 0} for index, task in enumerate(plan["taskIds"])]
        review = evaluate_pilot(plan, pilot_candidate, results)
        images = {task: {"imageDigest": "sha256:" + "a" * 64} for task in plan["taskIds"]}
        review["imageLocks"] = images
        review["imageLocksSha256"] = canonical_digest(images)
        review["evidenceSha256"] = canonical_digest({key: value for key, value in review.items() if key != "evidenceSha256"})
        full_candidate = CandidateBinding("1" * 40, "2" * 64, "4" * 64, "5" * 64)
        runner = object.__new__(BenchmarkRunner)
        runner.plan = {"taskIds": [*plan["taskIds"], *[f"task-{index}" for index in range(69)]], "attempts": 5}
        runner.candidate = full_candidate
        runner.image_preparer = SimpleNamespace(snapshot_locks=lambda: {"tasks": images})
        dispatched = []
        async def batch(stage, ids, *, attempts):
            dispatched.append((stage, len(ids), attempts))
            return {"stage": stage}
        runner._run_batch = batch
        with self.assertRaises(ValueError):
            await runner.run_full(review, {})
        self.assertEqual(dispatched, [])
        approval = {"schema": "centaeris.workspaceFullReview.v1", "decision": "approveFull",
            "reviewer": "Codex review under maintainer conditional authorization", "candidate": full_candidate.to_dict(),
            "pilotEvidenceSha256": review["evidenceSha256"]}
        await runner.run_full(review, approval)
        self.assertEqual(dispatched, [("full", 89, 5)])
        runner.image_preparer = SimpleNamespace(snapshot_locks=lambda: {"tasks": {}})
        with self.assertRaisesRegex(ValueError, "image lock"):
            await runner.run_full(review, approval)
        runner.candidate = CandidateBinding("1" * 40, "6" * 64, "4" * 64, "5" * 64)
        with self.assertRaises(ValueError):
            await runner.run_full(review, approval)

    async def test_cleanup_failure_stops_exclusive_waiter_without_cancelling_active_instance(self):
        admission = Admission(Resources(12000, 8, 100000), Resources(1000, 1, 1000))
        active = await admission.acquire("active", Resources(2048, 1, 1000))
        active.mark_creation_started()
        halt = asyncio.Event()
        pending = asyncio.create_task(_acquire_or_halt(admission, "exclusive", Resources(8192, 1, 1000), halt))
        await asyncio.sleep(0)
        halt.set()
        self.assertIsNone(await asyncio.wait_for(pending, 1))
        self.assertEqual(admission.active_count, 1)
        self.assertFalse(active.released)
        await active.release(cleanup_state="absent")

    async def test_cancel_racing_successful_admission_returns_never_created_capacity(self):
        admission = Admission(Resources(12000, 8, 100000), Resources(1000, 1, 1000))
        class Racing:
            async def acquire(self, identity, resources):
                allocation = await admission.acquire(identity, resources)
                asyncio.get_running_loop().call_soon(parent.cancel)
                return allocation
        parent = asyncio.create_task(_acquire_or_halt(Racing(), "race", Resources(2048, 1, 1000), asyncio.Event()))
        with self.assertRaises(asyncio.CancelledError):
            await parent
        self.assertEqual(admission.active_count, 0)


if __name__ == "__main__":
    unittest.main()
