"""Frozen Workspace benchmark plans, lifecycle admission, and review binding.

The admission object is process-local orchestration policy, not an execution
ledger. Callers must preserve its allocation until confirmed cleanup and reconcile
all existing instances before starting another process.
"""
from __future__ import annotations

import asyncio
from collections import deque
from dataclasses import dataclass
import hashlib
import json
import math
from pathlib import Path
import re
import subprocess
import tomllib
import unicodedata
from urllib.parse import urlsplit
from typing import Any

DATASET_COMMIT = '7131e4375048a0e408a8fb404b5f499d726b695b'
DATASET_URL = 'https://github.com/harbor-framework/terminal-bench-2-1.git'


def canonical_digest(value: Any) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':'),
                                    ensure_ascii=False, allow_nan=False).encode()).hexdigest()


def _keys(value: dict, expected: set[str], label: str) -> None:
    if not isinstance(value, dict) or set(value) != expected:
        raise ValueError(f'{label}: missing or unknown fields')


def _positive_int(value: Any, label: str) -> int:
    if type(value) is not int or value <= 0:
        raise ValueError(f'{label} must be a positive integer')
    return value


def _task_ids(ids: Any) -> list[str]:
    if not isinstance(ids, list) or not ids or any(
            not isinstance(item, str) or not re.fullmatch(r'[a-z0-9]+(?:-[a-z0-9.]+)*', item)
            or '..' in item for item in ids):
        raise ValueError('invalid taskIds')
    if len(ids) != len(set(ids)):
        raise ValueError('duplicate taskIds')
    return ids


def validate_pilot_selection(task_ids: Any, attempts: Any) -> list[str]:
    ids = _task_ids(task_ids)
    if not 1 <= len(ids) <= 20 or type(attempts) is not int or attempts != 1:
        raise ValueError('pilot requires one through twenty distinct tasks and one attempt')
    return ids


def validate_plan(value: dict, *, runnable: bool = False) -> dict:
    _keys(value, {'schema', 'name', 'dataset', 'taskIds', 'attempts',
                  'maxConcurrentTrials', 'exclusiveMemoryMiB', 'maxRetries',
                  'timeoutMultiplier', 'fullRunAuthorized', 'model'}, 'plan')
    if value['schema'] != 'centaeris.workspaceBenchmarkPlan.v1':
        raise ValueError('unsupported plan schema')
    if not isinstance(value['name'], str) or not value['name'].strip():
        raise ValueError('plan name is required')
    _keys(value['dataset'], {'gitUrl', 'commit'}, 'dataset')
    if value['dataset'] != {'gitUrl': DATASET_URL, 'commit': DATASET_COMMIT}:
        raise ValueError('unsupported dataset pin')
    ids = _task_ids(value['taskIds'])
    if not (len(ids) == 89 and type(value['attempts']) is int and value['attempts'] in (1, 5)):
        validate_pilot_selection(ids, value['attempts'])
    if (type(value['maxConcurrentTrials']) is not int or value['maxConcurrentTrials'] != 2
            or type(value['exclusiveMemoryMiB']) is not int or value['exclusiveMemoryMiB'] != 8192
            or type(value['maxRetries']) is not int or value['maxRetries'] != 0
            or isinstance(value['timeoutMultiplier'], bool) or value['timeoutMultiplier'] != 1
            or value['fullRunAuthorized'] is not False):
        raise ValueError('unsupported pilot execution policy')
    validate_model(value['model'], runnable=runnable)
    return value


def validate_model(model: dict, *, runnable: bool = False) -> dict:
    """Validate the one OpenAI-completions descriptor shared by plan and seed."""
    _keys(model, {'apiBase', 'credentialEnv', 'id', 'thinking', 'contextTokens', 'outputTokens'}, 'model')
    endpoint = model['apiBase']
    try:
        parsed = urlsplit(endpoint) if isinstance(endpoint, str) else None
        if (parsed is None or parsed.scheme != 'https' or not parsed.hostname
                or parsed.username is not None or parsed.password is not None
                or parsed.query or parsed.fragment or endpoint != endpoint.strip()
                or any(c.isspace() or ord(c) < 32 for c in endpoint)
                or parsed.port == 0):
            raise ValueError()
    except (ValueError, TypeError):
        raise ValueError('model apiBase must be an HTTPS endpoint without credentials, query or fragment') from None
    if (not isinstance(model['credentialEnv'], str)
            or re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*', model['credentialEnv']) is None):
        raise ValueError('model credentialEnv must be an environment variable name')
    name = model['id']
    if (not isinstance(name, str) or not name or len(name) > 256 or name != name.strip()
            or any(unicodedata.category(c).startswith('C') for c in name)):
        raise ValueError('model id is invalid')
    thinking = model['thinking']
    if thinking is not None and (not isinstance(thinking, str) or not thinking
            or len(thinking) > 64 or thinking != thinking.strip()
            or unicodedata.normalize('NFC', thinking) != thinking
            or any(unicodedata.category(c).startswith('C') for c in thinking)):
        raise ValueError('model thinking must be a valid mode or null')
    context = _positive_int(model['contextTokens'], 'contextTokens')
    output = model['outputTokens']
    if output is not None:
        _positive_int(output, 'outputTokens')
        if output >= context:
            raise ValueError('outputTokens must leave input context')
    if runnable and output is None:
        raise ValueError('outputTokens must be confirmed before a paid run')
    return model


def load_plan(path: Path) -> dict:
    def unique_pairs(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f'duplicate JSON field: {key}')
            result[key] = value
        return result
    value = json.loads(Path(path).read_text(encoding='utf-8-sig'), object_pairs_hook=unique_pairs)
    return validate_plan(value)


@dataclass(frozen=True)
class Resources:
    memory_mib: int
    cpus: float
    storage_mib: int

    def __post_init__(self):
        _positive_int(self.memory_mib, 'memoryMiB')
        _positive_int(self.storage_mib, 'storageMiB')
        if (isinstance(self.cpus, bool) or not isinstance(self.cpus, (int, float))
                or not math.isfinite(self.cpus) or self.cpus <= 0):
            raise ValueError('cpus must be positive and finite')


@dataclass(frozen=True)
class TaskResources:
    resources: Resources
    source_sha256: str
    allow_internet: bool

    def to_dict(self):
        return {'memoryMiB': self.resources.memory_mib, 'cpus': self.resources.cpus,
                'storageMiB': self.resources.storage_mib, 'sourceSha256': self.source_sha256,
                'allowInternet': self.allow_internet}


def _git(root: Path, *args: str) -> str:
    return subprocess.check_output(['git', '-C', str(root), *args], text=True,
                                   stderr=subprocess.PIPE).strip()


def _git_blob(root: Path, path: str) -> bytes:
    return subprocess.check_output(['git', '-C', str(root), 'show', f'{DATASET_COMMIT}:{path}'],
                                   stderr=subprocess.PIPE)


def load_task_resources(dataset_root: Path, task_ids: list[str]) -> dict[str, TaskResources]:
    """Read the real pinned checkout; unversioned Harbor cache is not a pin proof."""
    _task_ids(task_ids)
    root = Path(dataset_root).resolve(strict=True)
    if _git(root, 'rev-parse', 'HEAD') != DATASET_COMMIT:
        raise ValueError('dataset commit does not match fixed pin')
    paths = [f'tasks/{task}/task.toml' for task in task_ids]
    if _git(root, 'status', '--porcelain', '--untracked-files=all', '--', *paths):
        raise ValueError('task resource metadata has local modifications')
    found = {}
    for task, relative in zip(task_ids, paths):
        path = root / relative
        if not path.resolve(strict=True).is_relative_to(root) or path.is_symlink():
            raise ValueError('task metadata escapes checkout')
        payload = path.read_bytes()
        if payload != _git_blob(root, relative):
            raise ValueError('task metadata bytes differ from pinned source')
        environment = tomllib.loads(payload.decode('utf-8'))['environment']
        gpus = environment.get('gpus', 0)
        if type(gpus) is not int or gpus != 0:
            raise ValueError('GPU task requires a separately validated host')
        internet = environment.get('allow_internet', True)
        if type(internet) is not bool:
            raise ValueError('invalid allow_internet')
        resources = Resources(environment['memory_mb'], environment['cpus'], environment['storage_mb'])
        found[task] = TaskResources(resources, hashlib.sha256(payload).hexdigest(), internet)
    return found


class Allocation:
    def __init__(self, gate: 'Admission', identity: str, resources: Resources):
        self._gate = gate
        self.identity = identity
        self.resources = resources
        self.phase = 'setup'
        self.creation_started = False
        self.released = False

    def mark_creation_started(self) -> None:
        """Call BEFORE a create request, including one whose response could be lost."""
        if self.released:
            raise ValueError('allocation already released')
        self.creation_started = True

    async def release(self, *, cleanup_state: str) -> None:
        if cleanup_state != 'absent' and not (cleanup_state == 'neverCreated' and not self.creation_started):
            raise ValueError('release requires confirmed physical absence')
        await self._gate._release(self)


class Admission:
    """FIFO, atomic admission held from setup through verified cleanup.

    Memory includes the task's declared maximum, not its current sampled usage.
    Host totals are the measured budgets available to this evaluation; the reserve
    includes control services and safety headroom. Unobservable cleanup keeps the
    allocation held. Cancelling a waiter never cancels active allocations.
    """
    def __init__(self, total: Resources, control_reserve: Resources):
        self._budget = (total.memory_mib - control_reserve.memory_mib,
                        total.cpus - control_reserve.cpus,
                        total.storage_mib - control_reserve.storage_mib)
        if any(value <= 0 for value in self._budget):
            raise ValueError('control reserve exhausts resource budget')
        self._condition = asyncio.Condition()
        self._waiting = deque()
        self._active: dict[str, Allocation] = {}

    @property
    def active_count(self) -> int:
        return len(self._active)

    @property
    def active_storage_mib(self) -> int:
        """Declared storage still reserved through confirmed physical cleanup."""
        return sum(item.resources.storage_mib for item in self._active.values())

    def _fits(self, request: Resources) -> bool:
        if len(self._active) >= 2:
            return False
        if self._active and (request.memory_mib >= 8192 or any(
                value.resources.memory_mib >= 8192 for value in self._active.values())):
            return False
        used = (sum(item.resources.memory_mib for item in self._active.values()),
                sum(item.resources.cpus for item in self._active.values()),
                sum(item.resources.storage_mib for item in self._active.values()))
        return all(have + need <= limit for have, need, limit in zip(
            used, (request.memory_mib, request.cpus, request.storage_mib), self._budget))

    def _validate_request(self, identity: str, resources: Resources) -> None:
        if not isinstance(identity, str) or not identity:
            raise ValueError('trial identity required')
        if any(need > limit for need, limit in zip(
                (resources.memory_mib, resources.cpus, resources.storage_mib), self._budget)):
            raise ValueError('task exceeds available resource budget')

    async def try_acquire(self, identity: str, resources: Resources) -> Allocation | None:
        """Reserve only immediately available capacity; never bypass FIFO waiters."""
        self._validate_request(identity, resources)
        async with self._condition:
            if identity in self._active or identity in self._waiting:
                raise ValueError('duplicate trial identity')
            if self._waiting or not self._fits(resources):
                return None
            allocation = Allocation(self, identity, resources)
            self._active[identity] = allocation
            return allocation

    async def acquire(self, identity: str, resources: Resources) -> Allocation:
        self._validate_request(identity, resources)
        async with self._condition:
            if identity in self._active or identity in self._waiting:
                raise ValueError('duplicate trial identity')
            self._waiting.append(identity)
            try:
                while self._waiting[0] != identity or not self._fits(resources):
                    await self._condition.wait()
                self._waiting.popleft()
                allocation = Allocation(self, identity, resources)
                self._active[identity] = allocation
                self._condition.notify_all()
                return allocation
            except BaseException:
                self._waiting.remove(identity)
                self._condition.notify_all()
                raise

    async def _release(self, allocation: Allocation) -> None:
        async with self._condition:
            if allocation.released or self._active.get(allocation.identity) is not allocation:
                raise ValueError('allocation already released or wrong owner')
            del self._active[allocation.identity]
            allocation.released = True
            self._condition.notify_all()


@dataclass(frozen=True)
class CandidateBinding:
    product_commit: str
    adapter_sha256: str
    config_sha256: str
    task_set_sha256: str

    def __post_init__(self):
        for name, value, length in (
                ('productCommit', self.product_commit, 40), ('adapterSha256', self.adapter_sha256, 64),
                ('configSha256', self.config_sha256, 64), ('taskSetSha256', self.task_set_sha256, 64)):
            if not isinstance(value, str) or not re.fullmatch(r'[0-9a-f]{' + str(length) + '}', value):
                raise ValueError(f'invalid {name}')

    def to_dict(self) -> dict:
        return {'productCommit': self.product_commit, 'adapterSha256': self.adapter_sha256,
                'configSha256': self.config_sha256, 'taskSetSha256': self.task_set_sha256}


def make_binding(product_commit: str, adapter_files: dict[str, Path], plan: dict,
                 task_resources: dict[str, TaskResources]) -> CandidateBinding:
    validate_plan(plan)
    if set(task_resources) != set(plan['taskIds']):
        raise ValueError('resource inventory differs from configured tasks')
    if not adapter_files:
        raise ValueError('adapter sources required')
    sources = {name: hashlib.sha256(Path(path).read_bytes()).hexdigest()
               for name, path in adapter_files.items()}
    return CandidateBinding(product_commit, canonical_digest(sources), canonical_digest(plan),
                            canonical_digest({'dataset': plan['dataset'], 'tasks': {
                                name: resource.to_dict() for name, resource in task_resources.items()}}))


def _validate_outcome(value: Any, phase: str) -> None:
    _keys(value, {'status', 'error'}, phase + 'Outcome')
    statuses = {'absent', 'pending'} if phase == 'cleanup' else {'completed', 'failed', 'unknown'}
    if phase == 'verifier':
        statuses |= {'notStarted', 'running'}
    if not isinstance(value['status'], str) or value['status'] not in statuses:
        raise ValueError(f'invalid {phase} outcome status')
    error = value['error']
    if error is not None and (not isinstance(error, str) or not error or len(error) > 256
            or error != error.strip() or any(unicodedata.category(c).startswith('C') for c in error)):
        raise ValueError(f'invalid {phase} outcome error')
    if phase != 'cleanup' and (value['status'] == 'failed') != (error is not None):
        raise ValueError(f'inconsistent {phase} outcome error')


def evaluate_pilot(plan: dict, candidate: CandidateBinding, results: list[dict]) -> dict:
    validate_plan(plan)
    validate_pilot_selection(plan['taskIds'], plan['attempts'])
    issues = []
    if candidate.config_sha256 != canonical_digest(plan):
        issues.append('candidate configuration digest differs from pilot')
    expected = set(plan['taskIds'])
    seen_tasks, seen_trials = set(), set()
    fields = {'trialId', 'taskId', 'attempt', 'verifierCompleted', 'reward',
              'infrastructureError', 'severeIssue', 'cleanupState', 'resourceDebit',
              'agentOutcome', 'verifierOutcome', 'cleanupOutcome'}
    for result in results:
        try:
            _keys(result, fields, 'result')
            for phase in ('agent', 'verifier', 'cleanup'):
                _validate_outcome(result[phase + 'Outcome'], phase)
        except ValueError as error:
            issues.append(str(error))
            continue
        task, identity = result['taskId'], result['trialId']
        if not isinstance(task, str) or not isinstance(identity, str):
            issues.append('invalid task or trial identity')
            continue
        if task not in expected or task in seen_tasks:
            issues.append('unexpected or duplicate pilot task')
        if not isinstance(identity, str) or not identity or identity in seen_trials:
            issues.append('missing or duplicate trial identity')
        seen_tasks.add(task)
        seen_trials.add(identity)
        if type(result['attempt']) is not int or result['attempt'] != 1:
            issues.append('unexpected pilot attempt')
        reward = result['reward']
        if (result['verifierCompleted'] is not True or isinstance(reward, bool)
                or not isinstance(reward, (int, float)) or not math.isfinite(reward) or not 0 <= reward <= 1):
            issues.append(f'{task}: actual verifier reward missing or invalid')
        if result['verifierOutcome']['status'] != 'completed':
            issues.append(f'{task}: verifier outcome is incomplete')
        if result['infrastructureError'] is not None or result['severeIssue'] is not None:
            issues.append(f'{task}: unresolved infrastructure or severe issue')
        if (result['cleanupState'] != 'absent' or result['cleanupOutcome']['status'] != result['cleanupState']
                or type(result['resourceDebit']) is not int or result['resourceDebit'] != 0):
            issues.append(f'{task}: cleanup or resource reconciliation incomplete')
    if seen_tasks != expected or len(results) != len(plan['taskIds']):
        issues.append('one completed result for every selected pilot task required')
    report = {'schema': 'centaeris.workspacePilotReview.v1', 'candidate': candidate.to_dict(),
              'pilotConfigSha256': canonical_digest(plan), 'results': results,
              'status': 'blocked' if issues else 'readyForReview', 'issues': issues}
    report['evidenceSha256'] = canonical_digest(report)
    return report


def require_full_review(candidate: CandidateBinding, report: dict, approval: dict) -> None:
    """Validate an explicit maintainer review; readiness alone never starts 445."""
    _keys(approval, {'schema', 'decision', 'reviewer', 'candidate', 'pilotEvidenceSha256'}, 'approval')
    if (approval['schema'] != 'centaeris.workspaceFullReview.v1'
            or approval['decision'] != 'approveFull' or not isinstance(approval['reviewer'], str)
            or not approval['reviewer'].strip()):
        raise ValueError('explicit maintainer full-run approval required')
    if (report.get('schema') != 'centaeris.workspacePilotReview.v1'
            or report.get('status') != 'readyForReview' or report.get('issues') != []):
        raise ValueError('pilot is not ready for review')
    payload = {key: value for key, value in report.items() if key != 'evidenceSha256'}
    if canonical_digest(payload) != report.get('evidenceSha256'):
        raise ValueError('pilot report digest does not match content')
    results = report.get('results')
    if not isinstance(results, list) or len(results) != 20:
        raise ValueError('full review requires twenty distinct completed pilot task results')
    try:
        _task_ids([row.get('taskId') if isinstance(row, dict) else None for row in results])
    except ValueError:
        raise ValueError('full review requires twenty distinct completed pilot task results') from None
    pilot_candidate = report.get('candidate', {})
    if (pilot_candidate.get('productCommit') != candidate.product_commit
            or pilot_candidate.get('adapterSha256') != candidate.adapter_sha256):
        raise ValueError('product or adapter changed since pilot')
    if candidate.to_dict() != approval['candidate']:
        raise ValueError('review is stale for current full target candidate')
    if approval['pilotEvidenceSha256'] != report['evidenceSha256']:
        raise ValueError('review is stale for pilot evidence')
