"""Real Harbor trials through Workspace, with a separate one-task service gate.

This controller is imported by the private Linux bootstrap. It never serializes
authenticated clients or maintenance credentials into Harbor configuration.
"""
from __future__ import annotations

import asyncio
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import tarfile
import threading
import tempfile
import uuid

from harbor.models.task.task import Task
from harbor.models.trial.config import AgentConfig, EnvironmentConfig, TaskConfig, TrialConfig, VerifierConfig
from harbor.trial.hooks import TrialEvent
from harbor.trial.trial import Trial

from workspace_environment import WorkspaceEnvironment
from workspace_events import FailOpenProjection, JobEvents
from workspace_plan import DATASET_COMMIT, Admission, Resources, canonical_digest, evaluate_pilot, require_full_review, validate_plan, validate_pilot_selection
from workspace_provisioner import pack_upload

_REGISTRY = {}
_REGISTRY_LOCK = threading.Lock()


class DiskCapacityInsufficient(ValueError):
    """A pre-execution capacity deferral, never a model/trial failure."""
    def __init__(self, free_bytes, required_bytes, *, prepared_image=None):
        super().__init__('benchmark_disk_capacity_insufficient')
        self.free_bytes, self.required_bytes = free_bytes, required_bytes
        self.prepared_image = prepared_image

    def diagnostic(self):
        return {'freeBytes': self.free_bytes, 'requiredBytes': self.required_bytes}


def _git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args], stderr=subprocess.PIPE)


def freeze_tasks(dataset_root, task_ids):
    """Verify every selected blob, not merely resource metadata or Git status."""
    root = Path(dataset_root).resolve(strict=True)
    if _git(root, "rev-parse", "HEAD").decode().strip() != DATASET_COMMIT:
        raise ValueError("dataset commit differs from the fixed pin")
    if not task_ids or len(set(task_ids)) != len(task_ids) or any(
            not isinstance(name, str) or re.fullmatch(r"[a-z0-9][a-z0-9.-]*", name) is None or ".." in name for name in task_ids):
        raise ValueError("invalid task selection")
    selected = ["tasks/" + name for name in task_ids]
    if _git(root, "status", "--porcelain=v1", "--untracked-files=all", "--ignored", "--", *selected).strip():
        raise ValueError("selected task contains modified, untracked or ignored files")
    frozen = {}
    for task_id, directory in zip(task_ids, selected):
        files = {}
        for entry in _git(root, "ls-tree", "-rz", "--full-tree", DATASET_COMMIT, "--", directory).split(b"\0"):
            if not entry:
                continue
            header, raw_path = entry.split(b"\t", 1)
            mode, kind, object_id = header.decode().split()
            if mode not in {"100644", "100755"} or kind != "blob":
                raise ValueError("linked or non-file task source is unsupported")
            relative = raw_path.decode("utf-8")
            path = root / relative
            if path.is_symlink() or not path.resolve(strict=True).is_relative_to(root):
                raise ValueError("task source escapes the pinned checkout")
            for parent in path.parents:
                if parent == root:
                    break
                if parent.is_symlink() or (hasattr(parent, "is_junction") and parent.is_junction()):
                    raise ValueError("task source has a linked directory")
            payload = path.read_bytes()
            actual = hashlib.sha1(b"blob " + str(len(payload)).encode() + b"\0" + payload).hexdigest()
            if actual != object_id:
                raise ValueError("task bytes differ from the pinned Git blob")
            if os.name == "posix" and bool(path.stat().st_mode & 0o111) != (mode == "100755"):
                raise ValueError("task executable mode differs from the pinned Git tree")
            files[relative[len(directory) + 1:]] = {"mode": mode, "sha256": hashlib.sha256(payload).hexdigest(), "bytes": len(payload)}
        if not {"task.toml", "instruction.md"}.issubset(files) or not any(name.startswith("tests/") for name in files):
            raise ValueError("task source is incomplete")
        frozen[task_id] = {"files": files, "digest": canonical_digest(files)}
    return frozen


def validate_verifier_inputs(dataset_root, task_ids):
    """Exercise the actual bounded packer for every input before any model run."""
    with tempfile.TemporaryDirectory(prefix="workspace-verifier-preflight-") as temporary:
        archive = Path(temporary) / "tests.tar"
        for task_id in task_ids:
            pack_upload(Path(dataset_root) / "tasks" / task_id / "tests", archive, directory=True)


def _failure_code(error):
    """Keep stable codes/types, never exception messages or server bodies."""
    from workspace_client import WorkspaceError
    current = error
    seen = set()
    while current is not None and id(current) not in seen:
        seen.add(id(current))
        if isinstance(current, WorkspaceError) and re.fullmatch(r"[a-z][a-z0-9_]{0,127}", current.code):
            return current.code
        current = current.__cause__
    return type(error).__name__


def _observe_trial_failures(trial, outcomes):
    # Harbor 0.21 intentionally retains only its first exception_info. Observe
    # every attempted record before that policy suppresses later phase failures.
    original = trial._record_exception
    def record(error):
        outcomes["errors"].setdefault(outcomes["phase"], _failure_code(error))
        original(error)
    trial._record_exception = record
    return lambda: setattr(trial, "_record_exception", original)


def task_profile(task, image):
    config = task.config
    if (config.steps or config.environment.env or config.verifier.env or config.verifier.environment is not None
            or config.environment.gpus or config.environment.tpu or config.environment.mcp_servers
            or (config.verifier.environment_mode is not None and config.verifier.environment_mode.value != "shared")):
        raise ValueError("task requires an unsupported environment combination")
    resources = config.environment
    if any(type(value) is not int or value <= 0 for value in (resources.cpus, resources.memory_mb, resources.storage_mb)):
        raise ValueError("task resource declarations must be explicit")
    user = image["user"]
    # The image preparer resolves the task's requested agent user, falling back
    # to its original image USER. Never substitute a privileged user.
    if re.fullmatch(r"(?:0|[1-9][0-9]*):(?:0|[1-9][0-9]*)", user) is None:
        raise ValueError("image user has not been resolved")
    policy = config.environment.network_mode
    network = "disabled" if policy and policy.value == "no-network" else "publicInternet"
    if policy and policy.value not in {"public", "no-network"}:
        raise ValueError("network allowlists require separate support")
    return {"schema": "runtime.benchmark_execution_profile.v1", "imageDigest": image["imageDigest"],
        "workspaceRoot": image["workspaceRoot"], "user": user, "network": network, "resources": {
            "cpuMilli": resources.cpus * 1000, "memoryBytes": resources.memory_mb * 1024**2,
            "storageBytes": resources.storage_mb * 1024**2, "pidsLimit": 4096, "dataTmpfsBytes": 64 * 1024**2}}


class _TrialProjection:
    """One bounded observation merged with the current lifecycle, never a copy of it."""
    def __init__(self, sink, trial_id, phase, metrics_factory=None):
        self.sink, self.trial_id, self.phase = sink, trial_id, phase
        self.factory, self.sampler = metrics_factory, None
        self.lock = threading.RLock()
        self.closed = self.started = False

    def project(self, payload):
        with self.lock:
            update = dict(payload)
            if update.get("phase") == "setup":
                update["phase"] = "preparing"
            self.phase.update(update)
            if self.phase.get("resourceHolding") == 0 and "metrics" in self.phase:
                memory = self.phase["metrics"].get("memory", {})
                for key in ("taskCurrentBytes", "taskPeakBytes", "taskLimitBytes", "taskOomKills"):
                    if key in memory:
                        memory[key] = None
            self.sink.submit("trial", json.loads(json.dumps(self.phase)), trial_id=self.trial_id)

    def _observe(self, metrics):
        try:
            with self.lock:
                if not self.closed:
                    self.project({"metrics": json.loads(json.dumps(metrics, allow_nan=False))})
        except Exception:
            # Optional telemetry must not affect agent, verifier or cleanup.
            pass

    def start(self, environment):
        with self.lock:
            if self.started or self.closed or self.factory is None:
                return
            self.started = True
        try:
            owned = environment._owned
            def container():
                current = environment._owned
                if (environment._destroyed or current is None or any(
                        getattr(current, name) != getattr(owned, name) for name in
                        ("session_id", "agent_run_id", "instance_id", "container_id"))):
                    return None
                return current.container_id
            self.sampler = self.factory(owned.session_id, owned.agent_run_id, container, self._observe)
            self.sampler.start()
        except Exception:
            self.close()

    def close(self):
        with self.lock:
            self.closed = True
            sampler = self.sampler
        if sampler is not None:
            try:
                sampler.close()
            except Exception:
                pass


class RegisteredWorkspaceEnvironment(WorkspaceEnvironment):
    def __init__(self, *args, controllerBinding, **kwargs):
        with _REGISTRY_LOCK:
            entry = _REGISTRY.get(controllerBinding)
            if entry is None or entry.get("environment") is not None:
                raise ValueError("controller binding is missing or already consumed")
            if (Path(kwargs["environment_dir"]).resolve() != entry["environmentDir"]
                    or Path(kwargs["trial_paths"].trial_dir).name != entry["trialId"]):
                raise ValueError("controller binding belongs to another trial")
            super().__init__(*args, **kwargs, **entry["ports"])
            entry["environment"] = self
            self._controller_entry = entry

    async def start(self, force_build=False):
        await super().start(force_build=force_build)
        projection = self._controller_entry.get("metricsProjection")
        if projection is not None:
            projection.start(self)

    async def stop(self, delete=True):
        entry = self._controller_entry
        if "outcomes" in entry:
            entry["outcomes"]["phase"] = "cleanup"
        callback = entry.get("preDestroyCheck")
        if delete and callback is not None and not entry.get("serviceChecksRun"):
            entry["serviceChecksRun"] = True
            try:
                await callback(self)
            except Exception as error:
                entry["serviceCheckError"] = type(error).__name__
        # A failed observation must never prevent exact physical cleanup.
        await super().stop(delete=delete)


def native_trial_config(task_path, job_root, trial_id, controller_ref, model_ref, model):
    return TrialConfig(task=TaskConfig(path=Path(task_path)), trial_name=trial_id,
        trials_dir=Path(job_root) / "trials", timeout_multiplier=1,
        agent=AgentConfig(import_path="workspace_agent:WorkspaceAgent", model_name=model["id"],
            kwargs={"model_config_ref": model_ref, "thinking_mode": model["thinking"]}),
        environment=EnvironmentConfig(import_path="workspace_benchmark:RegisteredWorkspaceEnvironment",
            delete=True, force_build=False, kwargs={"controllerBinding": controller_ref}), verifier=VerifierConfig(disable=False))


async def _docker_capture(endpoint, args, *, timeout=120, limit=1024 * 1024):
    process = await asyncio.create_subprocess_exec("docker", "--host", endpoint, *args,
        stdin=asyncio.subprocess.DEVNULL, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL)
    async def read():
        output = bytearray()
        while block := await process.stdout.read(65536):
            output.extend(block)
            if len(output) > limit:
                raise ValueError("Docker metadata exceeds its bound")
        if await process.wait() != 0:
            raise ValueError("Docker metadata operation failed")
        return bytes(output)
    try:
        return await asyncio.wait_for(read(), timeout)
    except BaseException:
        if process.returncode is None:
            process.kill()
            await process.wait()
        raise


class DockerImagePreparer:
    """Pull/build original task images on the same explicit Linux Docker host."""
    def __init__(self, endpoint, image_lock_path=None):
        if not isinstance(endpoint, str) or not endpoint.startswith("unix:///"):
            raise ValueError("an explicit Linux Docker socket is required")
        self.endpoint = endpoint
        self.image_lock_path = Path(image_lock_path) if image_lock_path is not None else None
        self._image_locks = {"schema": "centaeris.workspaceBenchmarkImages.v1", "tasks": {}}
        self._prepare_locks = {}
        if self.image_lock_path is not None and self.image_lock_path.exists():
            if self.image_lock_path.is_symlink() or self.image_lock_path.stat().st_size > 1024 * 1024:
                raise ValueError("image lock file is unsafe or too large")
            value = json.loads(self.image_lock_path.read_text(encoding="utf-8"))
            if (not isinstance(value, dict) or set(value) != {"schema", "tasks"}
                    or value["schema"] != self._image_locks["schema"] or not isinstance(value["tasks"], dict)):
                raise ValueError("image lock schema invalid")
            self._image_locks = value

    def snapshot_locks(self):
        return json.loads(json.dumps(self._image_locks))

    def _save_lock(self):
        self.image_lock_path.parent.mkdir(parents=True, exist_ok=True)
        temporary = self.image_lock_path.with_name(self.image_lock_path.name + "." + uuid.uuid4().hex + ".tmp")
        try:
            _write(temporary, self._image_locks)
            os.replace(temporary, self.image_lock_path)
        finally:
            temporary.unlink(missing_ok=True)

    async def _inspect_image(self, image):
        format_value = '[{{json .Id}},{{json (index .Config "WorkingDir")}},{{json (index .Config "User")}},{{json .Os}},{{json .Architecture}},{{json .RepoDigests}}]'
        metadata = json.loads(await _docker_capture(self.endpoint, ["image", "inspect", "--format", format_value, image]))
        if isinstance(metadata, list) and len(metadata) == 6:
            # Docker omits unset optional image configuration fields. Preserve
            # explicit values; existing preparation applies root and / defaults.
            for index in (1, 2):
                if metadata[index] is None:
                    metadata[index] = ""
        if (not isinstance(metadata, list) or len(metadata) != 6 or not all(isinstance(item, str) for item in metadata[:5])
                or not re.fullmatch(r"sha256:[0-9a-f]{64}", metadata[0]) or metadata[3:5] != ["linux", "amd64"]
                or not isinstance(metadata[5], (list, type(None)))):
            raise ValueError("image metadata does not match the validated host")
        for digest in metadata[5] or []:
            if not isinstance(digest, str) or re.fullmatch(r"[A-Za-z0-9._:/-]+@sha256:[0-9a-f]{64}", digest) is None:
                raise ValueError("image repository digest invalid")
        return metadata

    async def _pull(self, image, task, log_root):
        Path(log_root).mkdir(parents=True, exist_ok=True)
        with (Path(log_root) / "image-pull.log").open("ab") as output:
            process = await asyncio.create_subprocess_exec("docker", "--host", self.endpoint, "pull", image,
                stdin=asyncio.subprocess.DEVNULL, stdout=output, stderr=output)
            try:
                await asyncio.wait_for(process.wait(), task.config.environment.build_timeout_sec)
            except BaseException:
                if process.returncode is None:
                    process.kill()
                    await process.wait()
                raise
            if process.returncode:
                raise ValueError("task image download failed")

    async def _identity_file(self, image, path, evidence_root):
        name = "benchmark-metadata-" + uuid.uuid4().hex
        evidence = Path(evidence_root) / (name + ".json")
        evidence.parent.mkdir(parents=True, exist_ok=True)
        _write(evidence, {"name": name, "imageDigest": image, "state": "pending"})
        container = None
        try:
            container = (await _docker_capture(self.endpoint, ["create", "--name", name,
                "--label", "centaeris.benchmark.metadata=" + name, "--network", "none",
                "--entrypoint", "/bin/true", image])).decode().strip()
            if re.fullmatch(r"[0-9a-f]{64}", container) is None:
                container = None
                raise ValueError("metadata container identity invalid")
            raw = await _docker_capture(self.endpoint, ["cp", container + ":" + path, "-"], limit=1024 * 1024)
            with tarfile.open(fileobj=io.BytesIO(raw), mode="r:") as archive:
                members = archive.getmembers()
                if len(members) != 1 or not members[0].isfile() or members[0].size > 512 * 1024:
                    raise ValueError("image identity archive invalid")
                return archive.extractfile(members[0]).read().decode("utf-8")
        finally:
            if container is None:
                # A create ACK can be lost. Inspect only this random, labelled
                # metadata intent; never clean up an arbitrary name collision.
                value = json.loads(await _docker_capture(self.endpoint, ["inspect", "--format",
                    '[{{json .Id}},{{json .Image}},{{json (index .Config.Labels "centaeris.benchmark.metadata")}}]', name]))
                if (not isinstance(value, list) or len(value) != 3 or value[1:] != [image, name]
                        or re.fullmatch(r"[0-9a-f]{64}", value[0]) is None):
                    raise ValueError("metadata cleanup identity is unconfirmed")
                container = value[0]
            await _docker_capture(self.endpoint, ["rm", "-v", container])
            _write(evidence, {"name": name, "imageDigest": image, "containerId": container, "state": "absent"})

    async def _user(self, image, value, evidence_root):
        value = str(value) if value is not None else ""
        if value in {"", "root", "0", "0:0", "root:root"}:
            return "0:0"
        if re.fullmatch(r"[0-9]+:[0-9]+", value):
            return ":".join(str(int(part)) for part in value.split(":"))
        username, separator, group = value.partition(":")
        passwd = await self._identity_file(image, "/etc/passwd", evidence_root)
        matches = [line.split(":") for line in passwd.splitlines() if len(line.split(":")) == 7
                   and (line.split(":")[0] == username or line.split(":")[2] == username)]
        if len(matches) != 1:
            raise ValueError("image USER cannot be resolved without changing it")
        uid, gid = matches[0][2:4]
        if separator:
            if group.isdecimal():
                gid = group
            else:
                groups = await self._identity_file(image, "/etc/group", evidence_root)
                found = [line.split(":")[2] for line in groups.splitlines() if len(line.split(":")) == 4 and line.split(":")[0] == group]
                if len(found) != 1:
                    raise ValueError("image group cannot be resolved")
                gid = found[0]
        if not uid.isdecimal() or not gid.isdecimal() or max(int(uid), int(gid)) >= 2**32 - 1:
            raise ValueError("invalid image identity")
        return f"{int(uid)}:{int(gid)}"

    async def prepare(self, task, log_root):
        if self.image_lock_path is None:
            raise ValueError("a shared public image lock path is required before preparation")
        lock = self._prepare_locks.setdefault(task.short_name, asyncio.Lock())
        async with lock:
            return await self._prepare(task, log_root)

    async def _prepare(self, task, log_root):
        from harbor.environments.docker.utils import ensure_docker_image_built
        environment = task.config.environment
        if (task.paths.environment_dir / "docker-compose.yaml").exists():
            raise ValueError("Compose tasks are not supported by this host profile")
        source_checksum = task.checksum
        locked = self._image_locks["tasks"].get(task.short_name)
        if locked is not None:
            required = {"sourceChecksum", "imageDigest", "workspaceRoot", "user", "originalImageUser", "imageReference", "repoDigests"}
            if not isinstance(locked, dict) or set(locked) != required or locked["sourceChecksum"] != source_checksum:
                raise ValueError("task source differs from its original image lock")
            try:
                metadata = await self._inspect_image(locked["imageDigest"])
            except ValueError:
                if not locked["repoDigests"]:
                    raise ValueError("locked local image is missing and has no immutable repository digest") from None
                await self._pull(locked["repoDigests"][0], task, log_root)
                metadata = await self._inspect_image(locked["imageDigest"])
            if (metadata[0] != locked["imageDigest"] or (metadata[1] or "/") != locked["workspaceRoot"]
                    or metadata[2] != locked["originalImageUser"]):
                raise ValueError("locked image metadata changed")
            return json.loads(json.dumps(locked))
        if environment.docker_image:
            image = environment.docker_image
            try:
                await _docker_capture(self.endpoint, ["image", "inspect", "--format", "{{.Id}}", image])
            except ValueError:
                await self._pull(image, task, log_root)
        else:
            # Harbor's own builder honors its content hash/cache/lock and leaves
            # the task image unmodified. Bind the explicit Docker host for this
            # serialized preparation; do not race a process environment switch.
            if os.environ.get("DOCKER_HOST") != self.endpoint:
                raise ValueError("Harbor source builds require matching explicit DOCKER_HOST")
            image = await ensure_docker_image_built(docker_name="workspace-benchmark-" + task.short_name,
                docker_build_context=task.paths.environment_dir, dockerfile_path=task.paths.environment_dir / "Dockerfile",
                build_args={}, platform="linux/amd64", timeout_sec=environment.build_timeout_sec)
        metadata = await self._inspect_image(image)
        user = task.config.agent.user if task.config.agent.user is not None else metadata[2]
        result = {"imageDigest": metadata[0], "workspaceRoot": metadata[1] or "/", "user": await self._user(metadata[0], user, log_root),
                  "originalImageUser": metadata[2], "imageReference": image, "repoDigests": sorted(metadata[5] or []),
                  "sourceChecksum": source_checksum}
        self._image_locks["tasks"][task.short_name] = result
        self._save_lock()
        return json.loads(json.dumps(result))


def _write(path, value):
    Path(path).write_text(json.dumps(value, sort_keys=True, indent=2, allow_nan=False), encoding="utf-8")


async def _acquire_or_halt(admission, identity, resources, halt):
    acquire = asyncio.create_task(admission.acquire(identity, resources))
    stopped = asyncio.create_task(halt.wait())
    try:
        done, _ = await asyncio.wait({acquire, stopped}, return_when=asyncio.FIRST_COMPLETED)
        if acquire in done:
            allocation = acquire.result()
            if halt.is_set():
                await allocation.release(cleanup_state="neverCreated")
                return None
            return allocation
        acquire.cancel()
        try:
            await acquire
        except asyncio.CancelledError:
            pass
        return None
    except BaseException:
        # Cancellation can arrive after acquire committed its process-local
        # reservation but before this helper returned it to the worker.
        if not acquire.done():
            acquire.cancel()
            await asyncio.gather(acquire, return_exceptions=True)
        if not acquire.cancelled() and acquire.exception() is None:
            allocation = acquire.result()
            if not allocation.released:
                await allocation.release(cleanup_state="neverCreated")
        raise
    finally:
        stopped.cancel()
        if not acquire.done():
            acquire.cancel()
        await asyncio.gather(acquire, stopped, return_exceptions=True)


def _preflight_passed(report):
    results, checks = report.get("results", []), report.get("serviceChecks", {})
    return (len(results) == 1 and isinstance(checks, dict)
        and results[0].get("verifierCompleted") is True
        and type(results[0].get("reward")) in {int, float} and results[0]["reward"] == 1
        and results[0].get("infrastructureError") is None and results[0].get("severeIssue") is None
        and results[0].get("cleanupState") == "absent" and type(results[0].get("resourceDebit")) is int
        and results[0]["resourceDebit"] == 0
        and all(checks.get(key) is True for key in ("actualToolEffects", "artifactDownload", "backgroundContinuity")))


class BenchmarkRunner:
    def __init__(self, *, plan, dataset_root, output_root, candidate, admission: Admission,
                 workspace_id, agent_id, model_ref, client_factory, provisioner, image_preparer,
                 physical_gate, sink_factory=None, batch_started=None, metrics_factory=None):
        validate_plan(plan, runnable=True)
        self.plan = json.loads(json.dumps(plan))
        self.dataset_root, self.output_root = Path(dataset_root).resolve(), Path(output_root)
        self.sources = freeze_tasks(self.dataset_root, plan["taskIds"])
        self.source_digest = canonical_digest(self.sources)
        self.candidate_digest = canonical_digest(candidate.to_dict())
        if candidate.config_sha256 != canonical_digest(self.plan):
            raise ValueError("candidate configuration digest differs from the approved plan")
        self.candidate = candidate
        self.admission, self.client_factory, self.provisioner = admission, client_factory, provisioner
        self.workspace_id, self.agent_id, self.model_ref = workspace_id, agent_id, model_ref
        self.image_preparer, self.physical_gate = image_preparer, physical_gate
        self.sink_factory = sink_factory
        self.batch_started = batch_started
        self.metrics_factory = metrics_factory

    async def run_preflight(self, task_id="kv-store-grpc", *, service_checks):
        validate_pilot_selection(self.plan["taskIds"], self.plan.get("attempts", 1))
        if task_id not in self.plan["taskIds"] or service_checks is None:
            raise ValueError("preflight requires a selected task and actual service checks")
        report = await self._run_batch("preflight", [task_id], service_checks=service_checks)
        report["status"] = "passed" if _preflight_passed(report) else "blocked"
        report["evidenceSha256"] = canonical_digest(report)
        if "batchDir" in report:
            _write(Path(report["batchDir"]) / "preflight-result.json", report)
        return report

    async def run_pilot(self, preflight=None, *, approval=None):
        validate_pilot_selection(self.plan["taskIds"], self.plan.get("attempts", 1))
        if approval is not None:
            expected = {'schema': 'centaeris.workspaceBenchmarkPilotApproval.v1', 'authorized': True,
                        'admission': 'directPilot', 'candidateDigest': self.candidate_digest,
                        'sourceDigest': self.source_digest, 'taskIds': list(self.plan['taskIds'])}
            if preflight is not None or approval != expected or approval.get('authorized') is not True:
                raise ValueError("direct pilot requires explicit approval of this exact candidate and task set")
        elif (not isinstance(preflight, dict)
                or preflight.get("stage") != "preflight" or preflight.get("status") != "passed"
                or preflight.get("sourceDigest") != self.source_digest
                or preflight.get("candidateDigest") != self.candidate_digest or not _preflight_passed(preflight)
                or preflight.get("evidenceSha256") != canonical_digest({key: value for key, value in preflight.items() if key != "evidenceSha256"})):
            raise ValueError("a passing preflight of this exact source and candidate is required")
        report = await self._run_batch("pilot", list(self.plan["taskIds"]))
        if approval is not None:
            report['admission'] = json.loads(json.dumps(approval))
            _write(Path(report['batchDir']) / 'batch-result.json', report)
        report["review"] = evaluate_pilot(self.plan, self.candidate, report["results"])
        inventory = self.image_preparer.snapshot_locks()["tasks"]
        report["review"]["imageLocks"] = {task_id: inventory[task_id] for task_id in self.plan["taskIds"] if task_id in inventory}
        if set(report["review"]["imageLocks"]) != set(self.plan["taskIds"]):
            report["review"]["status"] = "blocked"
            report["review"]["issues"].append("pilot image lock inventory is incomplete")
        report["review"]["imageLocksSha256"] = canonical_digest(report["review"]["imageLocks"])
        report["review"]["evidenceSha256"] = canonical_digest({key: value for key, value in report["review"].items() if key != "evidenceSha256"})
        _write(Path(report["batchDir"]) / "pilot-review.json", report["review"])
        return report

    async def run_full(self, pilot_review=None, approval=None):
        """Require a reviewed pilot or explicit approval of one direct full pass."""
        if len(self.plan["taskIds"]) != 89 or self.plan["attempts"] not in {1, 5}:
            raise ValueError("full run requires eighty-nine tasks and one or five attempts")
        if pilot_review is None:
            expected = {'schema': 'centaeris.workspaceBenchmarkFullApproval.v1', 'authorized': True,
                        'admission': 'directFull', 'candidateDigest': self.candidate_digest,
                        'sourceDigest': self.source_digest, 'taskIds': list(self.plan['taskIds']), 'attempts': 1}
            if (self.plan['attempts'] != 1 or not isinstance(approval, dict) or approval != expected
                    or approval.get('authorized') is not True or type(approval.get('attempts')) is not int):
                raise ValueError("direct full run requires explicit approval of this exact candidate, task set and single attempt")
            report = await self._run_batch('full', list(self.plan['taskIds']), attempts=1)
            report['admission'] = json.loads(json.dumps(approval))
            _write(Path(report['batchDir']) / 'batch-result.json', report)
            return report
        require_full_review(self.candidate, pilot_review, approval)
        previous = pilot_review.get("imageLocks")
        current = self.image_preparer.snapshot_locks()["tasks"]
        expected = {row["taskId"] for row in pilot_review["results"]}
        if (not isinstance(previous, dict) or set(previous) != expected or not expected.issubset(self.plan["taskIds"])
                or pilot_review.get("imageLocksSha256") != canonical_digest(previous)
                or any(current.get(task_id) != value for task_id, value in previous.items())):
            raise ValueError("full run must preserve every reviewed pilot image lock")
        return await self._run_batch("full", list(self.plan["taskIds"]), attempts=self.plan['attempts'])

    async def _run_batch(self, stage, task_ids, service_checks=None, attempts=1):
        if stage == 'pilot':
            try:
                validate_pilot_selection(task_ids, attempts)
            except ValueError:
                raise ValueError("batch shape differs from its authorized entrypoint") from None
            if task_ids != self.plan['taskIds']:
                raise ValueError("batch shape differs from its authorized entrypoint")
        elif (stage, len(task_ids), attempts) not in {("preflight", 1, 1), ("full", 89, 1), ("full", 89, 5)}:
            raise ValueError("batch shape differs from its authorized entrypoint")
        if platform.system() != "Linux":
            raise ValueError("the real controller must run on the validated Linux host")
        if not await self.physical_gate():
            raise ValueError("physical host and database gates have not passed")
        if freeze_tasks(self.dataset_root, self.plan["taskIds"]) != self.sources:
            raise ValueError("task source changed after candidate freezing")
        await asyncio.to_thread(validate_verifier_inputs, self.dataset_root, self.plan["taskIds"])
        root = self.output_root / (stage + "-" + uuid.uuid4().hex)
        root.mkdir(parents=True, exist_ok=False)
        manifest = {"schema": "centaeris.workspace_benchmark.job.v1", "jobId": root.name,
            "title": "Workspace " + stage, "taskCount": len(task_ids), "trialCount": len(task_ids) * attempts,
            "attemptsPerTask": attempts, "maxConcurrent": 2, "tasks": [{"taskId": task_id,
                "memoryMiB": Task(self.dataset_root / "tasks" / task_id).config.environment.memory_mb} for task_id in task_ids]}
        sink = self.sink_factory(root, manifest) if self.sink_factory else FailOpenProjection(JobEvents.create(root / "monitor", manifest))
        _write(root / "source-binding.json", {"candidate": self.candidate.to_dict(), "sources": self.sources,
            "sourceDigest": self.source_digest, "model": self.plan["model"], "stage": stage})
        sink.submit("job", {"status": "running"})
        if self.batch_started is not None:
            try:
                await self.batch_started({"stage": stage, "batchDir": str(root), "projectionDir": str(root / "monitor")})
            except Exception as error:
                sink.submit("log", {"level": "warning", "message": "Monitor attach unavailable: " + type(error).__name__})
        slots = asyncio.Queue()
        for slot in (1, 2):
            slots.put_nowait(slot)
        pending = [(task_id, attempt, task_id + "__" + uuid.uuid4().hex[:12], None)
                   for attempt in range(1, attempts + 1) for task_id in task_ids]
        results, checks, unrun = [], {}, []
        scheduling = asyncio.Condition()
        running, retained = 0, False

        async def next_trial():
            nonlocal running
            async with scheduling:
                while pending:
                    for index, (task_id, attempt, trial_id, prepared_image) in enumerate(pending):
                        task = Task(self.dataset_root / "tasks" / task_id)
                        resources = Resources(task.config.environment.memory_mb,
                            task.config.environment.cpus, task.config.environment.storage_mb)
                        # A live host shortage leaves the pending attempt intact.
                        # The allocation and this check have no intervening await
                        # that can admit a competing worker in this controller.
                        try:
                            if hasattr(self.image_preparer, 'check_capacity'):
                                self.image_preparer.check_capacity(resources.storage_mib)
                        except DiskCapacityInsufficient as error:
                            if retained:
                                continue
                            break
                        allocation = await self.admission.try_acquire(trial_id, resources)
                        if allocation is not None:
                            pending.pop(index)
                            running += 1
                            return task_id, attempt, task, trial_id, allocation, prepared_image
                        # Normally drain in order for exclusive tasks. If a
                        # completed trial still holds capacity, later tasks may
                        # use the capacity that is genuinely available.
                        if not retained:
                            break
                    if not running:
                        for task_id, attempt, _, _ in pending:
                            row = {"taskId": task_id, "attempt": attempt, "reason": "retainedCapacityUnavailable"}
                            if hasattr(self.image_preparer, 'check_capacity'):
                                task = Task(self.dataset_root / "tasks" / task_id)
                                try:
                                    self.image_preparer.check_capacity(task.config.environment.storage_mb)
                                except DiskCapacityInsufficient as error:
                                    row.update(reason="diskCapacityUnavailable", **error.diagnostic())
                            unrun.append(row)
                        pending.clear()
                        return None
                    await scheduling.wait()
                return None

        async def worker():
            nonlocal running, retained
            while True:
                selected = await next_trial()
                if selected is None:
                    return
                task_id, attempt, task, trial_id, allocation, prepared_image = selected
                slot = await slots.get()
                controller_ref = uuid.uuid4().hex
                entry, trial, result = None, None, None
                restore_trial_observer = None
                outcomes = {"phase": "setup", "errors": {}, "started": set(), "finished": set()}
                capacity_deferred = False
                phase = {"taskId": task_id, "attempt": attempt, "status": "running", "phase": "preparing", "slot": slot, "resourceHolding": 1}
                projection = _TrialProjection(sink, trial_id, phase, getattr(self, "metrics_factory", None))
                project = projection.project
                project({})
                try:
                    if freeze_tasks(self.dataset_root, [task_id])[task_id] != self.sources[task_id]:
                        raise ValueError("task source changed before image preparation")
                    # No pull or source build occurs before this allocation.
                    image = prepared_image if prepared_image is not None else await self.image_preparer.prepare(task, root / "images" / trial_id)
                    if freeze_tasks(self.dataset_root, [task_id])[task_id] != self.sources[task_id]:
                        raise ValueError("task source changed during image preparation")
                    profile = task_profile(task, image)
                    if hasattr(self.image_preparer, "snapshot_locks"):
                        _write(root / "image-locks.json", self.image_preparer.snapshot_locks())
                    _write(root / (trial_id + "-image.json"), {"image": image, "profile": profile})
                    client = self.client_factory()
                    entry = {"environmentDir": task.paths.environment_dir.resolve(), "trialId": trial_id,
                        "ports": {"workspace_client": client, "provisioner": self.provisioner, "allocation": allocation,
                            "workspace_id": self.workspace_id, "agent_id": self.agent_id, "trial_id": trial_id,
                            "profile": profile, "model_config_ref": self.model_ref, "thinking_mode": self.plan["model"]["thinking"],
                            "workspace_event_callback": project}, "environment": None, "metricsProjection": projection,
                        "outcomes": outcomes}
                    with _REGISTRY_LOCK:
                        _REGISTRY[controller_ref] = entry
                    if service_checks is not None:
                        async def before_destroy(environment):
                            checks.update(await service_checks(trial.result, environment))
                        entry["preDestroyCheck"] = before_destroy
                    config = native_trial_config(task.paths.task_dir, root, trial_id, controller_ref, self.model_ref, self.plan["model"])
                    trial = await Trial.create(config)
                    restore_trial_observer = _observe_trial_failures(trial, outcomes)
                    async def hook(event):
                        if event.event == TrialEvent.AGENT_END:
                            outcomes["finished"].add("agent")
                            return
                        outcomes["phase"] = "verifier" if event.event == TrialEvent.VERIFICATION_START else "agent"
                        outcomes["started"].add(outcomes["phase"])
                        project({"phase": outcomes["phase"]})
                    trial.add_hook(TrialEvent.AGENT_START, hook)
                    trial.add_hook(TrialEvent.AGENT_END, hook)
                    trial.add_hook(TrialEvent.VERIFICATION_START, hook)
                    try:
                        result = await trial.run()
                    finally:
                        if restore_trial_observer is not None:
                            restore_trial_observer()
                            restore_trial_observer = None
                except DiskCapacityInsufficient as error:
                    # Only image preparation can defer: never replay an admitted
                    # Workspace create, model request, or uncertain side effect.
                    if trial is not None or allocation.creation_started:
                        raise
                    capacity_deferred = True
                    prepared_image = error.prepared_image
                    with (root / 'capacity-observations.jsonl').open('a', encoding='utf-8') as observations:
                        observations.write(json.dumps({'taskId': task_id, 'trialId': trial_id,
                            'attempt': attempt, **error.diagnostic()}) + '\n')
                except Exception as error:
                    project({"error": _failure_code(error)})
                finally:
                    environment = entry["environment"] if entry else None
                    if environment is not None and not allocation.released:
                        try:
                            await environment.stop(delete=True)
                        except Exception as error:
                            outcomes["errors"].setdefault("cleanup", _failure_code(error))
                    if environment is None and not allocation.creation_started and not allocation.released:
                        await allocation.release(cleanup_state="neverCreated")
                    if restore_trial_observer is not None:
                        restore_trial_observer()
                    projection.close()
                    if capacity_deferred:
                        project({"phase": "queued", "status": "queued", "resourceHolding": 0})
                    else:
                        summary = self._summary(task_id, trial_id, result, allocation, phase.get("error"), attempt=attempt, outcomes=outcomes)
                        if entry and entry.get("serviceCheckError"):
                            summary["infrastructureError"] = entry["serviceCheckError"]
                        results.append(summary)
                        _write(root / (trial_id + "-summary.json"), summary)
                        project({"phase": "finished" if allocation.released else "cleanup",
                            "status": "completed" if summary["infrastructureError"] is None and summary["severeIssue"] is None else "failed",
                            "cleanupStatus": summary["cleanupState"], "resourceHolding": summary["resourceDebit"],
                            "verifierStatus": "completed" if summary["verifierCompleted"] else "unknown", "reward": summary["reward"]})
                    if allocation.released:
                        with _REGISTRY_LOCK:
                            _REGISTRY.pop(controller_ref, None)
                        slots.put_nowait(slot)
                    async with scheduling:
                        running -= 1
                        if capacity_deferred:
                            pending.insert(0, (task_id, attempt, trial_id, prepared_image))
                        retained = retained or not allocation.released
                        scheduling.notify_all()
        await asyncio.gather(worker(), worker())
        report = {"schema": "centaeris.workspaceBenchmarkBatch.v1", "stage": stage, "taskIds": task_ids,
            "sourceDigest": self.source_digest, "candidateDigest": self.candidate_digest,
            "results": results, "unrun": unrun, "serviceChecks": checks, "batchDir": str(root), "projectionDir": str(root / "monitor"),
            "attemptsPerTask": attempts,
            "status": "blocked" if unrun or len(results) != len(task_ids) * attempts
                or any(row["infrastructureError"] or row["severeIssue"] for row in results) else "finished"}
        inventory_path = root / "image-locks.json"
        report["imageInventorySha256"] = hashlib.sha256(inventory_path.read_bytes()).hexdigest() if inventory_path.exists() else None
        _write(root / "batch-result.json", report)
        sink.submit("job", {"status": "failed" if report["status"] == "blocked" else "completed"})
        sink.close()
        return report

    @staticmethod
    def _summary(task_id, trial_id, result, allocation, error, *, attempt=1, outcomes=None):
        rewards = result.verifier_result.rewards if result and result.verifier_result else None
        reward = rewards.get("reward") if isinstance(rewards, dict) else None
        if type(reward) not in {int, float} or not 0 <= reward <= 1:
            reward = None
        exception = result.exception_info.exception_type if result and result.exception_info else None
        outcomes = outcomes or {"errors": {}, "started": set(), "finished": set()}
        errors = outcomes["errors"]
        model_errors = {"AgentTimeoutError", "NonZeroAgentExitCodeError"}
        agent_error = errors.get("agent") or (exception if exception in model_errors else None)
        verifier_error = errors.get("verifier")
        infrastructure = error or verifier_error or errors.get("setup") or (
            agent_error if agent_error not in model_errors else None) or (
            exception if exception not in {None, *model_errors} else None)
        infrastructure = infrastructure or ("verifierResultMissing" if reward is None else None)
        return {"trialId": trial_id, "taskId": task_id, "attempt": attempt, "verifierCompleted": reward is not None,
            "reward": reward, "infrastructureError": infrastructure,
            "agentOutcome": {"status": "failed" if agent_error else "completed" if "agent" in outcomes["finished"] else "unknown", "error": agent_error},
            "verifierOutcome": {"status": "failed" if verifier_error else "completed" if reward is not None else "unknown", "error": verifier_error},
            "cleanupOutcome": {"status": "absent" if allocation.released else "pending", "error": errors.get("cleanup")},
            "severeIssue": None if allocation.released else "cleanupUnconfirmed",
            "cleanupState": "absent" if allocation.released else "pending", "resourceDebit": 0 if allocation.released else 1}
