"""Explicit, Linux-only entrypoints for a separately provisioned Workspace benchmark.

Private configuration, account passwords, and maintenance authority stay outside
Harbor jobs. This controller never seeds accounts, starts services, or retries an
uncertain trial. Gate logs are attestations of actual separately executed gates;
hash verification does not itself rerun those tests.
"""
from __future__ import annotations

import argparse
import asyncio
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import stat
import subprocess
import sys
from urllib.parse import urlsplit

from workspace_benchmark import BenchmarkRunner, DockerImagePreparer, DiskCapacityInsufficient, _docker_capture
from workspace_client import WorkspaceClient
from workspace_plan import Admission, Resources, CandidateBinding, canonical_digest, load_plan, load_task_resources, make_binding
from workspace_provisioner import WorkspaceHTTPProvisioner

class LaunchError(Exception):
    """Fixed public failure code; never attach private values or HTTP bodies."""


class CapacityCheckedImages:
    """Recheck real free bytes including all currently admitted task promises."""
    def __init__(self, images, filesystem, reserve_mib, active_storage_mib):
        self.images, self.filesystem = images, filesystem
        self.reserve_mib, self.active_storage_mib = reserve_mib, active_storage_mib

    def snapshot_locks(self):
        return self.images.snapshot_locks()

    def check_capacity(self, additional_storage_mib=0, *, prepared_image=None):
        needed = (self.reserve_mib + self.active_storage_mib() + additional_storage_mib) * 1024**2
        free = shutil.disk_usage(self.filesystem).free
        if free < needed:
            raise DiskCapacityInsufficient(free, needed, prepared_image=prepared_image)

    async def prepare(self, task, log_root):
        self.check_capacity()
        image = await self.images.prepare(task, log_root)
        self.check_capacity(prepared_image=image)
        return image


async def _metric_container(identity, manifest, role):
    endpoint = 'unix://' + identity['dockerSocket']
    project = manifest['project']
    _require(project == identity['fixtureId'])
    output = await _docker_capture(endpoint, ['ps', '--no-trunc', '--filter',
        'label=com.docker.compose.project=' + project, '--filter',
        'label=com.docker.compose.service=' + role, '--format', '{{.ID}}'], timeout=3)
    ids = output.decode('ascii').splitlines()
    _require(len(ids) == 1 and re.fullmatch('[0-9a-f]{64}', ids[0]))
    template = '{"id":{{json .Id}},"image":{{json .Image}},"running":{{json .State.Running}},"project":{{json (index .Config.Labels "com.docker.compose.project")}},"service":{{json (index .Config.Labels "com.docker.compose.service")}}}'
    value = json.loads(await _docker_capture(endpoint, ['inspect', '--type', 'container', '--format', template, ids[0]], timeout=3))
    _require(value == {'id': ids[0], 'image': manifest['images'][role]['id'], 'running': True, 'project': project, 'service': role})
    return ids[0]


async def metric_factory(identity, manifest, fixture_root):
    """Observation is optional; missing telemetry cannot acquire execution authority."""
    try:
        from workspace_metrics import DockerProbe, CgroupSampler, OwnedUsageReader, TrialMetricsSampler
        runtime_id = await _metric_container(identity, manifest, 'runtime')
        postgres_id = await _metric_container(identity, manifest, 'postgres')
        memory = CgroupSampler(DockerProbe(identity['dockerSocket']))
        usage = OwnedUsageReader(fixture_root / 'benchmark-fixture.json', postgres_id)
        def factory(session, run, task_id, callback):
            return TrialMetricsSampler(session, run, runtime_id, task_id, callback,
                usage_reader=usage, memory_sampler=memory, interval_seconds=10)
        return factory
    except Exception:
        return None


def validate_adapter_sources(value):
    _require(isinstance(value, dict))
    root = Path(__file__).resolve().parent
    required = {'workspace_' + name + '.py' for name in ('agent', 'benchmark', 'client', 'environment',
        'events', 'launch', 'metrics', 'monitor', 'plan', 'provisioner')}
    _require(required.issubset(value), 'adapter_source_inventory_incomplete')
    for name in required:
        _require(isinstance(value[name], str) and Path(value[name]).resolve() == root / name, 'adapter_source_path_invalid')
    paths = {key: Path(path) for key, path in value.items()}
    _require(all(path.is_absolute() and path.is_file() and not path.is_symlink() for path in paths.values()))
    return paths


def _require(condition, code='benchmark_input_invalid'):
    if not condition:
        raise LaunchError(code)


def _keys(value, keys):
    _require(isinstance(value, dict) and set(value) == set(keys))


def _json(path):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            _require(key not in result)
            result[key] = value
        return result
    _require(Path(path).stat().st_size <= 8 * 1024**2)
    return json.loads(Path(path).read_text(encoding='utf-8'), object_pairs_hook=unique)


def _sha(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as source:
        for chunk in iter(lambda: source.read(1024**2), b''):
            digest.update(chunk)
    return digest.hexdigest()


def private_mode_ok(mode):
    return stat.S_IMODE(mode) == 0o600


def require_private_file(path):
    path = Path(path)
    _require(path.is_absolute() and not path.is_symlink() and path.is_file(), 'private_file_invalid')
    _require(private_mode_ok(path.stat().st_mode) and path.stat().st_uid == os.getuid(), 'private_file_permissions')


def read_credentials(path):
    require_private_file(path)
    selected = {}
    for line in Path(path).read_text().splitlines():
        key, separator, value = line.partition('=')
        if key not in {'INTERNAL_API_TOKEN', 'ENVIRONMENT_MAINTENANCE_TOKEN'}:
            continue
        _require(separator and key not in selected and value and not any(c.isspace() for c in value), 'private_credentials_invalid')
        selected[key] = value
    _require(set(selected) == {'INTERNAL_API_TOKEN', 'ENVIRONMENT_MAINTENANCE_TOKEN'}, 'private_credentials_missing')
    return selected['INTERNAL_API_TOKEN'], selected['ENVIRONMENT_MAINTENANCE_TOKEN']


def verify_physical_gate(proof, candidate, source_tree, artifacts):
    _keys(proof, {'schema', 'candidate', 'sourceCommit', 'sourceTree', 'artifacts', 'gates'})
    _require(proof['schema'] == 'centaeris.workspaceBenchmarkPhysicalGate.v1'
        and proof['candidate'] == candidate.to_dict() and proof['sourceCommit'] == candidate.product_commit
        and proof['sourceTree'] == source_tree, 'physical_gate_binding_invalid')
    _keys(proof['artifacts'], artifacts)
    for name, expected_path in artifacts.items():
        item = proof['artifacts'][name]
        _keys(item, {'path', 'sha256'})
        _require(Path(item['path']).resolve() == Path(expected_path).resolve()
            and not Path(item['path']).is_symlink() and _sha(expected_path) == item['sha256'], 'physical_gate_artifact_changed')
    _require(isinstance(proof['gates'], list) and proof['gates'])
    seen = set()
    for gate in proof['gates']:
        _keys(gate, {'id', 'sourceCommit', 'exitCode', 'executedTests', 'testIdentities',
            'command', 'evidencePath', 'evidenceSha256'})
        identities, command = gate['testIdentities'], gate['command']
        _require(isinstance(gate['id'], str) and gate['id'] and gate['id'] not in seen
            and isinstance(gate['sourceCommit'], str) and re.fullmatch('[0-9a-f]{40}', gate['sourceCommit'])
            and type(gate['exitCode']) is int and gate['exitCode'] == 0
            and type(gate['executedTests']) is int and gate['executedTests'] > 0
            and isinstance(identities, list) and len(identities) == gate['executedTests']
            and all(isinstance(item, str) and item and '\n' not in item for item in identities)
            and len(set(identities)) == len(identities)
            and isinstance(command, list) and command and all(isinstance(item, str) and item for item in command), 'physical_gate_tests_invalid')
        path = Path(gate['evidencePath'])
        _require(path.is_absolute() and path.stat().st_size <= 32 * 1024**2 and not path.is_symlink()
            and _sha(path) == gate['evidenceSha256'], 'physical_gate_log_changed')
        log = path.read_text(encoding='utf-8')
        _require(shlex.join(command) in log and all(identity in log for identity in identities), 'physical_gate_log_incomplete')
        seen.add(gate['id'])
    return True


_SERVICE_SCRIPT = '''import glob,hashlib,importlib.metadata,json,os,socket
paths=['/app/kv-store.proto','/app/server.py']+glob.glob('/app/*_pb2.py')+glob.glob('/app/*_pb2_grpc.py')
files={p:hashlib.sha256(open(p,'rb').read(1048577)).hexdigest() for p in paths if os.path.isfile(p) and os.path.getsize(p)<=1048576}
packages={}
for name in ('grpcio','grpcio-tools'):
 try: packages[name]=importlib.metadata.version(name)
 except importlib.metadata.PackageNotFoundError: packages[name]=None
try:
 connection=socket.create_connection(('127.0.0.1',5328),timeout=2);connection.close();alive=True
except OSError: alive=False
print(json.dumps(dict(files=files,packages=packages,port5328=alive)))'''


async def service_checks(_result, environment):
    """Observe the official task's artifacts and live service after its verifier."""
    owned = environment._require_owned()
    observed = await asyncio.to_thread(environment.provisioner.observe_setup,
        owned.session_id, owned.environment_id, owned.agent_run_id)
    fields = ('session_id', 'environment_id', 'agent_run_id', 'instance_id', 'container_id', 'image_digest', 'profile_digest')
    _require(observed is not None and all(getattr(observed, key) == getattr(owned, key) for key in fields), 'preflight_instance_changed')
    result = await environment.exec('python3 -c ' + shlex.quote(_SERVICE_SCRIPT), timeout_sec=15)
    _require(result.return_code == 0 and len(result.stdout.encode()) <= 65536, 'preflight_observation_failed')
    value = json.loads(result.stdout)
    _keys(value, {'files', 'packages', 'port5328'})
    files = value['files']
    _require(isinstance(files, dict) and len(files) <= 64 and all(isinstance(key, str)
        and re.fullmatch(r'/app/[A-Za-z0-9_.-]+', key) and isinstance(digest, str)
        and re.fullmatch('[0-9a-f]{64}', digest) for key, digest in files.items()), 'preflight_observation_invalid')
    root = environment.trial_paths.artifacts_dir
    folder = root / 'service-check'
    folder.mkdir(parents=True, exist_ok=False)
    downloaded = False
    if '/app/server.py' in files:
        target = folder / 'server.py'
        await environment.download_file('/app/server.py', target)
        downloaded = target.is_file() and _sha(target) == files['/app/server.py']
    effects = ('/app/kv-store.proto' in files and '/app/server.py' in files
        and any(key.endswith('_pb2.py') for key in files) and any(key.endswith('_pb2_grpc.py') for key in files)
        and value['packages'] == {'grpcio': '1.73.0', 'grpcio-tools': '1.73.0'})
    checks = {'actualToolEffects': effects, 'artifactDownload': downloaded, 'backgroundContinuity': value['port5328'] is True}
    (root / 'service-check.json').write_text(json.dumps({'schema': 'centaeris.workspaceServiceCheck.v1',
        'sessionId': owned.session_id, 'agentRunId': owned.agent_run_id, 'environmentId': owned.environment_id,
        'instanceId': owned.instance_id, 'containerId': owned.container_id, 'observations': value, 'checks': checks}, indent=2))
    return checks


def _git(source, revision):
    return subprocess.check_output(['git', '-C', str(source), 'rev-parse', revision], stderr=subprocess.DEVNULL, text=True).strip()


def _origin(value, expected_port):
    parsed = urlsplit(value)
    _require(parsed.scheme == 'http' and parsed.hostname in {'127.0.0.1', 'localhost', '::1'}
        and parsed.port == expected_port and not parsed.username and not parsed.password
        and parsed.path in {'', '/'} and not parsed.query and not parsed.fragment, 'fixture_origin_invalid')
    return value


def load_controller(path):
    require_private_file(path)
    config = _json(path)
    _keys(config, {'schema', 'fixtureRoot', 'seedPath', 'planPath', 'datasetRoot', 'outputRoot',
        'imageLockPath', 'apiOrigin', 'runtimeOrigin', 'dockerSocket', 'physicalGatePath',
        'candidate', 'adapterSources', 'resources'})
    _require(config['schema'] == 'centaeris.workspaceBenchmarkController.v1')
    for key in ('fixtureRoot', 'seedPath', 'planPath', 'datasetRoot', 'outputRoot', 'imageLockPath', 'physicalGatePath'):
        _require(isinstance(config[key], str) and Path(config[key]).is_absolute() and not Path(config[key]).is_symlink())
    output = Path(config['outputRoot']).resolve()
    for secret in (Path(path), Path(config['seedPath']), Path(config['fixtureRoot']) / 'synthetic.env', Path(config['fixtureRoot']) / 'benchmark-fixture.json'):
        _require(not secret.resolve().is_relative_to(output), 'private_input_inside_jobs')
    return config


def check_previous_batches(output, stage):
    """A fresh process cannot forget an outstanding reservation or repeat work."""
    output = Path(output)
    if not output.exists():
        return
    for batch in output.iterdir():
        if not batch.is_dir():
            continue
        _require(not batch.is_symlink() and (batch / 'batch-result.json').is_file(), 'previous_batch_unresolved')
        report = _json(batch / 'batch-result.json')
        _require(report.get('stage') in {'preflight', 'pilot', 'full'} and report['stage'] != stage
            and report.get('status') == 'finished' and isinstance(report.get('results'), list)
            and report['results'] and all(row.get('cleanupState') == 'absent'
                and type(row.get('resourceDebit')) is int and row['resourceDebit'] == 0
                and row.get('infrastructureError') is None and row.get('severeIssue') is None
                for row in report['results']), 'previous_batch_unresolved_or_repeated')


@contextmanager
def _controller_lock(config):
    import fcntl
    lock = Path(config['imageLockPath']).with_suffix('.controller.lock')
    _require(lock.parent.is_dir() and not lock.is_symlink(), 'controller_lock_invalid')
    descriptor = os.open(lock, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise LaunchError('controller_already_running') from None
        yield
    finally:
        os.close(descriptor)


async def launch(config_path, stage, *, preflight_report=None, pilot_review=None, approval=None):
    _require(platform.system() == 'Linux', 'linux_controller_required')
    config = load_controller(config_path)
    with _controller_lock(config):
        check_previous_batches(config['outputRoot'], stage)
        return await _launch_config(config, stage, preflight_report=preflight_report, pilot_review=pilot_review, approval=approval)


async def _launch_config(config, stage, *, preflight_report=None, pilot_review=None, approval=None):
    fixture_root = Path(config['fixtureRoot'])
    identity_path = fixture_root / 'benchmark-fixture.json'
    require_private_file(identity_path)
    identity, manifest = _json(identity_path), _json(fixture_root / 'manifest.json')
    _keys(identity, {'schema', 'fixtureId', 'databaseName', 'databaseUser', 'sourceRevision', 'sourceTree', 'dockerHostId', 'dockerSocket'})
    _require(identity['schema'] == 'centaeris.workspaceBenchmarkFixture.v1' and re.fullmatch(r'tbworkspace[0-9a-f]{10}', identity['fixtureId'])
        and identity['fixtureId'] == identity['databaseName'] == identity['databaseUser']
        and manifest['project'] == identity['fixtureId']
        and identity['dockerSocket'] == config['dockerSocket'] == manifest['dockerSocket']
        and identity['dockerHostId'] == manifest['dockerHostId'] and manifest['benchmarkMode'] is True, 'fixture_identity_invalid')
    seed_path = Path(config['seedPath'])
    require_private_file(seed_path)
    seed = _json(seed_path)
    _keys(seed, {'schema', 'fixtureId', 'sourceRevision', 'email', 'password', 'workspaceId', 'agentId', 'modelConfigRef', 'model', 'modelSha256'})
    _require(seed['schema'] == 'centaeris.workspaceBenchmarkSeed.v1' and seed['fixtureId'] == identity['fixtureId']
        and seed['sourceRevision'] == identity['sourceRevision'] == manifest['sourceRevision']
        and all(isinstance(seed[key], str) and seed[key] for key in ('email','password','workspaceId','agentId','modelConfigRef')), 'seed_identity_invalid')
    plan = load_plan(Path(config['planPath']))
    _require(seed['model'] == plan['model']
        and seed['modelSha256'] == canonical_digest(plan['model']), 'seed_model_mismatch')
    resources = load_task_resources(Path(config['datasetRoot']), plan['taskIds'])
    candidate_wire = config['candidate']
    _keys(candidate_wire, {'productCommit', 'adapterSha256', 'configSha256', 'taskSetSha256'})
    candidate = CandidateBinding(candidate_wire['productCommit'], candidate_wire['adapterSha256'], candidate_wire['configSha256'], candidate_wire['taskSetSha256'])
    adapter_sources = validate_adapter_sources(config['adapterSources'])
    _require(make_binding(candidate.product_commit, adapter_sources, plan, resources) == candidate, 'candidate_source_changed')
    source = Path(manifest['source'])
    _require(_git(source, 'HEAD') == candidate.product_commit == identity['sourceRevision']
        and _git(source, 'HEAD^{tree}') == identity['sourceTree'] == manifest['buildManifest']['sourceTree'], 'product_source_changed')
    _require(subprocess.run(['git', '-C', str(source), 'diff', '--quiet', 'HEAD'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0, 'product_source_dirty')
    artifacts = {'runtimeServer': Path(manifest['binaryDirectory']) / 'runtime_server',
        'benchmarkHelper': Path(manifest['benchmarkHelper']['path']), 'serviceConfig': fixture_root / 'service.env'}
    _require(all(_sha(Path(manifest['binaryDirectory']) / name) == digest for name, digest in manifest['binarySha256'].items())
        and _sha(artifacts['benchmarkHelper']) == manifest['benchmarkHelper']['sha256']
        and manifest['benchmarkHelper']['sourceRevision'] == identity['sourceRevision']
        and manifest['benchmarkHelper']['sourceTree'] == identity['sourceTree'], 'fixture_binary_changed')
    def gate():
        return verify_physical_gate(_json(config['physicalGatePath']), candidate, identity['sourceTree'], artifacts)
    gate()  # Nothing below can authenticate, pull an image, or submit work before this.
    _keys(config['resources'], {'total', 'reserve'})
    def resource(item):
        _keys(item, {'memoryMiB', 'cpus', 'storageMiB'})
        return Resources(item['memoryMiB'], item['cpus'], item['storageMiB'])
    total, reserve = resource(config['resources']['total']), resource(config['resources']['reserve'])
    output = Path(config['outputRoot'])
    _require(output.parent.is_dir(), 'output_parent_missing')
    _require(total.memory_mib <= os.sysconf('SC_PHYS_PAGES') * os.sysconf('SC_PAGE_SIZE') // 1024**2
        and total.cpus <= (os.cpu_count() or 1)
        and total.storage_mib <= shutil.disk_usage(output.parent).free // 1024**2, 'host_capacity_changed')
    internal, maintenance = read_credentials(fixture_root / 'synthetic.env')
    api = _origin(config['apiOrigin'], manifest['apiPort'])
    runtime = _origin(config['runtimeOrigin'], manifest['runtimePort'])
    def client_factory():
        client = WorkspaceClient(api)
        client.login(seed['email'], seed['password'])
        return client
    async def physical_gate():
        return await asyncio.to_thread(gate)
    async def batch_started(value):
        path = Path(value['batchDir']) / 'batch-start.json'
        path.write_text(json.dumps(value, indent=2))
        print(json.dumps(value), flush=True)
    admission = Admission(total, reserve)
    metrics = await metric_factory(identity, manifest, fixture_root)
    runner = BenchmarkRunner(plan=plan, dataset_root=Path(config['datasetRoot']), output_root=output,
        candidate=candidate, admission=admission, workspace_id=seed['workspaceId'], agent_id=seed['agentId'],
        model_ref=seed['modelConfigRef'], client_factory=client_factory,
        provisioner=WorkspaceHTTPProvisioner(api_origin=api, runtime_origin=runtime, maintenance_token=maintenance, internal_token=internal),
        image_preparer=CapacityCheckedImages(DockerImagePreparer('unix://' + config['dockerSocket'], image_lock_path=Path(config['imageLockPath'])),
            output.parent, reserve.storage_mib, lambda: admission.active_storage_mib),
        physical_gate=physical_gate, batch_started=batch_started, metrics_factory=metrics)
    if stage == 'preflight':
        _require(not any((preflight_report, pilot_review, approval)))
        return await runner.run_preflight(service_checks=service_checks)
    if stage == 'pilot':
        _require(pilot_review is None and ((preflight_report is None) != (approval is None)))
        if approval is not None:
            return await runner.run_pilot(approval=_json(approval))
        return await runner.run_pilot(_json(preflight_report))
    _require(stage == 'full' and preflight_report is None and approval is not None)
    if pilot_review is None:
        return await runner.run_full(approval=_json(approval))
    return await runner.run_full(_json(pilot_review), _json(approval))


class _Parser(argparse.ArgumentParser):
    def error(self, message):
        raise LaunchError('launcher_arguments_invalid')


def main(argv=None):
    parser = _Parser(description=__doc__)
    parser.add_argument('--config', required=True)
    parser.add_argument('--stage', choices=('preflight', 'pilot', 'full'), required=True)
    parser.add_argument('--preflight-report')
    parser.add_argument('--pilot-review')
    parser.add_argument('--approval')
    try:
        args = parser.parse_args(argv)
    except LaunchError:
        print('launcher_arguments_invalid', file=sys.stderr)
        return 2
    try:
        result = asyncio.run(launch(Path(args.config), args.stage, preflight_report=args.preflight_report,
            pilot_review=args.pilot_review, approval=args.approval))
        return 0 if result.get('status') not in {'blocked', 'failed'} else 1
    except BaseException as error:
        if isinstance(error, (KeyboardInterrupt, SystemExit)):
            print('benchmark_controller_interrupted', file=sys.stderr)
        else:
            code = str(error) if isinstance(error, LaunchError) and re.fullmatch('[a-z_]{1,80}', str(error)) else 'benchmark_controller_failed'
            print(code, file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
