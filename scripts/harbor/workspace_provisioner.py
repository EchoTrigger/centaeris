"""Private controller HTTP transport and bounded task-file transfer.

Controller credentials stay in memory outside Harbor configuration and task
containers. Exec is dispatched once; uncertain external effects are not retried.
"""
from __future__ import annotations

import hashlib
import http.client
import json
import math
import os
from pathlib import Path, PurePosixPath
import re
import shlex
import shutil
import struct
import tarfile
import tempfile
import time
import uuid
from urllib.error import HTTPError, URLError

from workspace_client import WorkspaceClient, WorkspaceError, _segment
from workspace_environment import CleanupObservation, OwnedEnvironment

_TRANSFER_CONTRACT = json.loads(Path(__file__).with_name("benchmark-transport.json").read_text(encoding="utf-8"))
if (set(_TRANSFER_CONTRACT) != {"schema", "archiveBytes", "expandedBytes", "members", "metadataBytes"}
        or _TRANSFER_CONTRACT["schema"] != "centaeris.benchmarkTransport.v1"
        or any(type(value) is not int or value <= 0 for key, value in _TRANSFER_CONTRACT.items() if key != "schema")):
    raise ValueError("benchmark transport contract invalid")
ARCHIVE_LIMIT = _TRANSFER_CONTRACT["archiveBytes"]
EXPANDED_LIMIT = _TRANSFER_CONTRACT["expandedBytes"]
MEMBER_LIMIT = _TRANSFER_CONTRACT["members"]
TRANSFER_BLOCK_BYTES = 64 * 1024
RETIREMENT_ATTEMPTS = 3
FRAME_HEADER_LIMIT = 1024 * 1024


def _content_digest(source):
    digest = hashlib.sha256()
    with Path(source).open('rb') as stream:
        for block in iter(lambda: stream.read(TRANSFER_BLOCK_BYTES), b''):
            digest.update(block)
    return 'sha256:' + digest.hexdigest()


def _read_exact(source, count):
    parts = bytearray()
    while len(parts) < count:
        block = source.read(count - len(parts))
        if not block:
            raise WorkspaceError('transfer_response_incomplete')
        parts.extend(block)
    return bytes(parts)


def _read_frame(response, destination):
    if response.headers.get('Content-Type') != 'application/octet-stream':
        raise WorkspaceError('transfer_response_invalid')
    size = struct.unpack('!I', _read_exact(response, 4))[0]
    if not 0 < size <= FRAME_HEADER_LIMIT:
        raise WorkspaceError('transfer_metadata_limit_exceeded')
    metadata = json.loads(_read_exact(response, size))
    if not isinstance(metadata, dict) or set(metadata) != {'result', 'contentLength', 'contentDigest'}:
        raise WorkspaceError('transfer_response_invalid')
    length = metadata['contentLength']
    if type(length) is not int or not 0 <= length <= EXPANDED_LIMIT:
        raise WorkspaceError('transfer_expanded_limit_exceeded')
    result = metadata['result']
    if not isinstance(result, dict) or set(result) != {'output', 'error'}:
        raise WorkspaceError('transfer_response_invalid')
    output = result['output']
    if ((output is None) == (result['error'] is None)
            or output is not None and (not isinstance(output, dict) or len(output) != 1
                or not set(output) <= {'read_file', 'write_file', 'list_directory'})):
        raise WorkspaceError('transfer_response_invalid')
    read = output.get('read_file') if isinstance(output, dict) else None
    if (read is not None and (not isinstance(read, dict) or read.get('bytes') != [])
            or read is None and length != 0 or length and destination is None):
        raise WorkspaceError('transfer_response_invalid')
    digest = hashlib.sha256()
    stream = Path(destination).open('wb') if destination is not None else None
    try:
        remaining = length
        while remaining:
            block = _read_exact(response, min(TRANSFER_BLOCK_BYTES, remaining))
            digest.update(block)
            if stream is not None:
                stream.write(block)
            remaining -= len(block)
        if (metadata['contentDigest'] != 'sha256:' + digest.hexdigest() or response.read(1)
                or read is not None and read.get('fileHash') != 'sha256:' + digest.hexdigest()):
            raise WorkspaceError('transfer_response_digest_mismatch')
    finally:
        if stream is not None:
            stream.close()
    return result


class _LimitedWriter:
    def __init__(self, output, limit):
        self.output, self.limit, self.written = output, limit, 0

    def write(self, data):
        if self.written + len(data) > self.limit:
            raise WorkspaceError("transfer_archive_limit_exceeded")
        count = self.output.write(data)
        self.written += count
        return count

    def tell(self):
        return self.written


def _safe_name(name):
    if (not isinstance(name, str) or not name or "\\" in name or "\x00" in name
            or name.startswith("/") or any(part == ".." for part in name.split("/"))
            or any(ord(character) < 32 for character in name)):
        raise WorkspaceError("transfer_archive_path_invalid")
    path = PurePosixPath(name)
    reserved = {"CON", "PRN", "AUX", "NUL", *(f"COM{i}" for i in range(1, 10)), *(f"LPT{i}" for i in range(1, 10))}
    if (path.is_absolute() or any(":" in part or part.endswith((".", " "))
            or part.split(".", 1)[0].upper() in reserved for part in path.parts)):
        raise WorkspaceError("transfer_archive_path_invalid")
    return path


def _safe_local(path):
    path = Path(path).absolute()
    for component in (path, *path.parents):
        if component.is_symlink() or (hasattr(component, "is_junction") and component.is_junction()):
            raise WorkspaceError("transfer_local_link_rejected")
    return path


def _archive_members(archive, *, max_members, max_expanded_bytes):
    members, names, total = [], {}, 0
    for member in archive:
        if len(members) >= max_members:
            raise WorkspaceError("transfer_member_limit_exceeded")
        name = _safe_name(member.name)
        if (not member.isfile() and not member.isdir()) or member.issparse():
            raise WorkspaceError("transfer_archive_entry_rejected")
        if not name.parts and not member.isdir():
            raise WorkspaceError("transfer_archive_entry_rejected")
        if str(name) in names:
            raise WorkspaceError("transfer_archive_duplicate")
        for parent in name.parents:
            if str(parent) in names and not names[str(parent)]:
                raise WorkspaceError("transfer_archive_parent_invalid")
        if member.isfile() and any(existing.startswith(str(name) + "/") for existing in names):
            raise WorkspaceError("transfer_archive_parent_invalid")
        total += member.size
        if member.size < 0 or total > max_expanded_bytes:
            raise WorkspaceError("transfer_expanded_limit_exceeded")
        names[str(name)] = member.isdir()
        members.append(member)
    return members


def pack_upload(source, archive_path, *, directory, archive_name=None,
                max_archive_bytes=ARCHIVE_LIMIT, max_expanded_bytes=EXPANDED_LIMIT, max_members=MEMBER_LIMIT):
    source = _safe_local(source)
    if source.is_symlink() or (directory and not source.is_dir()) or (not directory and not source.is_file()):
        raise WorkspaceError("transfer_upload_source_invalid")
    entries = []
    if directory:
        for current, directories, files in os.walk(source, followlinks=False):
            for name in sorted(directories + files):
                path = _safe_local(Path(current) / name)
                if path.is_symlink() or (not path.is_dir() and not path.is_file()):
                    raise WorkspaceError("transfer_upload_source_invalid")
                entries.append((path, path.relative_to(source).as_posix()))
                if len(entries) > max_members:
                    raise WorkspaceError("transfer_member_limit_exceeded")
    else:
        entries.append((source, archive_name or source.name))
    expanded = sum(path.stat().st_size for path, _ in entries if path.is_file())
    if expanded > max_expanded_bytes:
        raise WorkspaceError("transfer_expanded_limit_exceeded")
    archive_path = _safe_local(archive_path)
    try:
        with archive_path.open("wb") as output:
            with tarfile.open(fileobj=_LimitedWriter(output, max_archive_bytes), mode="w", format=tarfile.PAX_FORMAT) as archive:
                current_expanded = 0
                for path, name in entries:
                    _safe_name(name)
                    info = archive.gettarinfo(path, arcname=name)
                    if not info.isfile() and not info.isdir():
                        raise WorkspaceError("transfer_upload_source_invalid")
                    current_expanded += info.size
                    if current_expanded > max_expanded_bytes:
                        raise WorkspaceError("transfer_expanded_limit_exceeded")
                    if info.isfile():
                        with path.open("rb") as source_file:
                            archive.addfile(info, source_file)
                    else:
                        archive.addfile(info)
        if archive_path.stat().st_size > max_archive_bytes:
            raise WorkspaceError("transfer_archive_limit_exceeded")
    except BaseException:
        archive_path.unlink(missing_ok=True)
        raise


def unpack_download(archive_path, target, *, directory, strip_root=None,
                    max_archive_bytes=ARCHIVE_LIMIT, max_expanded_bytes=EXPANDED_LIMIT, max_members=MEMBER_LIMIT):
    archive_path, target = _safe_local(archive_path), _safe_local(target)
    if archive_path.stat().st_size > max_archive_bytes:
        raise WorkspaceError("transfer_archive_limit_exceeded")
    try:
        with tarfile.open(archive_path, "r:") as archive:
            members = _archive_members(archive, max_members=max_members, max_expanded_bytes=max_expanded_bytes)
            planned = []
            for member in members:
                name = _safe_name(member.name)
                if strip_root is not None:
                    root = _safe_name(strip_root)
                    if len(root.parts) != 1 or not name.is_relative_to(root):
                        raise WorkspaceError("transfer_archive_root_invalid")
                    name = name.relative_to(root)
                if directory:
                    destination = target.joinpath(*name.parts)
                else:
                    if not member.isfile() or len(members) != 1:
                        raise WorkspaceError("transfer_file_archive_invalid")
                    destination = target
                _safe_local(destination)
                planned.append((member, destination))
            if not directory and not planned:
                raise WorkspaceError("transfer_file_archive_invalid")
            # All headers, limits, links and existing destination parents were
            # validated before writing any extracted task content.
            for member, destination in planned:
                if member.isdir():
                    destination.mkdir(parents=True, exist_ok=True)
                    continue
                destination.parent.mkdir(parents=True, exist_ok=True)
                descriptor, temporary = tempfile.mkstemp(prefix=".workspace-transfer-", dir=destination.parent)
                try:
                    with os.fdopen(descriptor, "wb") as output, archive.extractfile(member) as input_file:
                        shutil.copyfileobj(input_file, output, length=64 * 1024)
                    os.chmod(temporary, member.mode & 0o777)
                    os.utime(temporary, (member.mtime, member.mtime))
                    os.replace(temporary, destination)
                finally:
                    Path(temporary).unlink(missing_ok=True)
    except (tarfile.TarError, OSError, ValueError):
        raise WorkspaceError("transfer_archive_invalid") from None


class _MaintenanceHTTP(WorkspaceClient):
    def __init__(self, origin, maintenance_token, internal_token=None, *, request_timeout=10):
        super().__init__(origin, request_timeout=request_timeout)
        self.__maintenance_token, self.__internal_token = maintenance_token, internal_token

    def post(self, path, value, *, timeout=None):
        request = self._request("POST", path, value)
        if len(request.data) > 12 * 1024 * 1024:
            raise WorkspaceError("control_request_too_large")
        return self._send(request, timeout=timeout)

    def filesystem(self, value, *, source=None, destination=None):
        header = json.dumps(value, ensure_ascii=False, separators=(',', ':')).encode()
        if len(header) > FRAME_HEADER_LIMIT:
            raise WorkspaceError('transfer_metadata_limit_exceeded')
        prefix = struct.pack('!I', len(header)) + header
        def chunks():
            yield prefix
            if source is not None:
                with Path(source).open('rb') as stream:
                    yield from iter(lambda: stream.read(TRANSFER_BLOCK_BYTES), b'')
        request = self._request('POST', '/internal/environments/maintenance/filesystem',
            headers={'Content-Type': 'application/octet-stream'})
        request.data = chunks()
        request.add_header('Content-Length', str(len(prefix) + value['request']['contentLength']))
        return self._send(request, frame_destination=destination, framed=True)

    def _send(self, request, *, timeout=None, frame_destination=None, framed=False):
        request.add_header("X-Environment-Maintenance-Token", self.__maintenance_token)
        if self.__internal_token is not None:
            request.add_header("X-Internal-Token", self.__internal_token)
        try:
            with self.opener.open(request, timeout=timeout or self.request_timeout) as response:
                if framed:
                    return _read_frame(response, frame_destination)
                body = response.read(12 * 1024 * 1024 + 1)
                if len(body) > 12 * 1024 * 1024:
                    raise WorkspaceError("control_response_too_large")
                value = json.loads(body)
                if not isinstance(value, dict):
                    raise WorkspaceError("control_response_invalid")
                return value
        except HTTPError as error:
            with error:
                try:
                    payload = json.loads(error.read(4096))
                    code = payload.get("error") if isinstance(payload, dict) else None
                except (ValueError, UnicodeError):
                    code = None
            if not isinstance(code, str) or re.fullmatch(r"[a-z][a-z0-9_]{0,127}", code) is None:
                code = "control_http_error"
            raise WorkspaceError(code, error.code) from None
        except (URLError, OSError, http.client.HTTPException):
            raise WorkspaceError("transport_unavailable") from None
        except (ValueError, UnicodeError):
            raise WorkspaceError("control_response_invalid") from None


def _fields(value, schema, fields):
    if not isinstance(value, dict) or set(value) != {"schema", *fields} or value["schema"] != schema:
        raise WorkspaceError("control_response_invalid")


def _task_text(value, maximum):
    try:
        return (isinstance(value, str) and 0 < len(value.encode("utf-8")) <= maximum
            and not any(ord(character) < 32 or 127 <= ord(character) < 160 for character in value))
    except UnicodeError:
        return False


def _task_cursor(value):
    return type(value) is int and 0 <= value < 2**64


def _task_page(result, operation_id, scope, *, task_id=None, cursor=0):
    """Validate a Core page before consuming output or forwarding its cursor.

    The first task address is opaque. Subsequent reads keep that exact address,
    original operation and scope; this client does not derive Core task IDs.
    """
    if (not isinstance(result, dict)
            or set(result) != {"kind", "task", "output", "earliestCursor", "nextCursor", "gap", "hasMore"}
            or result["kind"] != "task"):
        raise WorkspaceError("maintenance_task_output_unconfirmed")
    task = result["task"]
    if (not isinstance(task, dict)
            or set(task) != {"taskId", "operationId", "scope", "state", "exitCode", "cleanupComplete", "outputComplete"}
            or not _task_text(task["taskId"], 128)
            or task_id is not None and task["taskId"] != task_id
            or task["operationId"] != operation_id or task["scope"] != scope
            or not isinstance(task["state"], str) or task["state"] not in {"accepted", "running", "exited", "unknown"}
            or type(task["cleanupComplete"]) is not bool or type(task["outputComplete"]) is not bool
            or task["exitCode"] is not None and (type(task["exitCode"]) is not int
                or not -(2**31) <= task["exitCode"] < 2**31)):
        raise WorkspaceError("maintenance_task_output_unconfirmed")
    if (not _task_cursor(result["earliestCursor"]) or not _task_cursor(result["nextCursor"])
            or result["earliestCursor"] > cursor or result["nextCursor"] < cursor
            or result["gap"] is not False or type(result["hasMore"]) is not bool
            or result["hasMore"] and result["nextCursor"] == cursor
            or not isinstance(result["output"], list)):
        raise WorkspaceError("maintenance_task_output_unconfirmed")
    previous, size = None, 0
    for chunk in result["output"]:
        if (not isinstance(chunk, dict) or set(chunk) != {"cursor", "stream", "data"}
                or not _task_cursor(chunk["cursor"])
                or not cursor <= chunk["cursor"] < result["nextCursor"]
                or previous is not None and chunk["cursor"] <= previous
                or not isinstance(chunk["stream"], str) or chunk["stream"] not in {"stdout", "stderr"}
                or not isinstance(chunk["data"], list)
                or any(type(byte) is not int or not 0 <= byte <= 255 for byte in chunk["data"])):
            raise WorkspaceError("maintenance_task_output_invalid")
        previous = chunk["cursor"]
        size += len(chunk["data"])
        if size > TRANSFER_BLOCK_BYTES:
            raise WorkspaceError("maintenance_task_output_invalid")
    return task


class WorkspaceHTTPProvisioner:
    """Independent maintenance authority; instantiate only in the controller.

    This object is deliberately injected in-process rather than serialized into
    Harbor job JSON. Public cookie sessions use a different WorkspaceClient.
    """
    def __init__(self, *, api_origin, runtime_origin, maintenance_token, internal_token, request_timeout=10):
        if (any(not isinstance(value, str) or len(value) < 32 for value in (maintenance_token, internal_token))
                or maintenance_token == internal_token):
            raise ValueError("independent maintenance and internal credentials are required")
        self.api = _MaintenanceHTTP(api_origin, maintenance_token, request_timeout=request_timeout)
        self.runtime = _MaintenanceHTTP(runtime_origin, maintenance_token, internal_token, request_timeout=request_timeout)
        self._retirement_requests = {}
        self._accepted_retirements = set()
        self._profiles = {}
        self._provision_requests = {}

    def bind_profile(self, session_id, environment_id, profile, operation_id):
        request = {"schema": "workspace.environment_profile.bind.v1", "operationId": operation_id,
                   "sessionId": session_id, "profile": profile}
        path = f"/internal/environments/{_segment(environment_id)}/environment-profile"
        try:
            value = self.api.post(path, request)
        except WorkspaceError as error:
            if error.code != "transport_unavailable":
                raise
            value = self.api.post(path, request)
        _fields(value, "workspace.environment_profile.bound.v1",
                {"operationId", "sessionId", "environmentId", "profileDigest", "profile"})
        expected_digest = self._digest(profile)
        if (value["operationId"] != operation_id or value["sessionId"] != session_id
                or value["environmentId"] != environment_id or value["profileDigest"] != expected_digest
                or value["profile"] != profile):
            raise WorkspaceError("environment_profile_binding_mismatch")
        self._profiles[environment_id] = profile
        return value

    @staticmethod
    def _digest(value):
        return "sha256:" + hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False,
            separators=(",", ":"), allow_nan=False).encode()).hexdigest()

    def _observe(self, session_id, environment_id):
        value = self.runtime.post("/internal/environments/observe", {
            "schema": "runtime.environment.observe.v1", "sessionId": session_id, "environmentId": environment_id})
        _fields(value, "runtime.environment.observe.result.v1", {"state", "sessionId", "environmentId",
            "instanceId", "containerId", "hostId", "controllerEpoch", "controllerOwner", "profile", "profileDigest", "resourceHolding"})
        if (value["sessionId"] != session_id or value["environmentId"] != environment_id
                or value["state"] not in {"unallocated", "allocationPending", "resident", "destroyed", "unknown"}
                or type(value["controllerEpoch"]) is not int or value["controllerEpoch"] < 0
                or not isinstance(value["profile"], dict) or self._digest(value["profile"]) != value["profileDigest"]
                or value["resourceHolding"] is not None and (type(value["resourceHolding"]) is not int
                    or value["resourceHolding"] not in {0, 1})):
            raise WorkspaceError("environment_observation_invalid")
        if environment_id in self._profiles and self._profiles[environment_id] != value["profile"]:
            raise WorkspaceError("environment_profile_binding_mismatch")
        self._profiles[environment_id] = value["profile"]
        # These are local projections of authoritative state, never extra wire fields.
        value["physicalState"] = "present" if value["state"] == "resident" else "absent" if value["state"] == "destroyed" else "unknown"
        value["imageDigest"] = value["profile"]["imageDigest"]
        return value

    def observe_setup(self, session_id, environment_id, agent_run_id=None):
        value = self._observe(session_id, environment_id)
        if value["state"] == "destroyed":
            raise WorkspaceError("environment_destroyed")
        if value["state"] != "resident":
            return None
        try:
            return OwnedEnvironment(session_id, environment_id, agent_run_id, value["instanceId"],
                value["containerId"], value["hostId"], value["controllerEpoch"], value["imageDigest"],
                value["profileDigest"], "resident", value["resourceHolding"])
        except ValueError:
            raise WorkspaceError("environment_observation_invalid") from None

    def provision(self, session_id, environment_id, operation_id):
        observed = self._observe(session_id, environment_id)
        if observed["state"] == "resident" and observed["controllerOwner"] is None:
            return self.observe_setup(session_id, environment_id)
        request = self._provision_requests.get(operation_id)
        if request is None:
            previous = observed["controllerEpoch"]
            if observed["controllerOwner"] == {"kind": "maintenanceOperation", "operationId": operation_id}:
                previous -= 1
            request = {"schema": "workspace.environment_provision.issue.v1", "operationId": operation_id,
                "sessionId": session_id, "expectedControllerEpoch": previous}
            self._provision_requests[operation_id] = request
        grant = self.api.post(f"/internal/environments/{_segment(environment_id)}/provision-authorizations", request)
        self._check_grant(grant, environment_id, operation_id, 'provision')
        payload = grant['payload']
        logical = {'kind': 'provisionEnvironment', **{name: payload[name] for name in
            ('environmentProfile', 'environmentProfileDigest', 'userId', 'agentId', 'pluginActivation')}}
        if self._digest(logical) != payload['requestDigest']:
            raise WorkspaceError('provision_request_binding_mismatch')
        result = self.runtime.post('/internal/environments/provision', {'authorization': grant, 'request': logical})
        _fields(result, 'runtime.environment.provision.result.v1',
            {'operationId', 'environmentId', 'instanceId', 'containerId', 'profileDigest', 'status'})
        if (result['operationId'] != operation_id or result['environmentId'] != environment_id
                or result['profileDigest'] != payload['environmentProfileDigest'] or result['status'] != 'resident'):
            raise WorkspaceError('provision_response_binding_mismatch')
        self._release(grant)
        owned = self.observe_setup(session_id, environment_id)
        if owned is None or owned.instance_id != result['instanceId'] or owned.container_id != result['containerId']:
            raise WorkspaceError('provision_response_binding_mismatch')
        return owned

    @staticmethod
    def _check_grant(grant, environment_id, operation_id, kind):
        if (not isinstance(grant, dict) or set(grant) != {'payload', 'signature'}
                or not isinstance(grant['payload'], dict) or not isinstance(grant['signature'], str)
                or grant['payload'].get('operationKind') != kind
                or grant['payload'].get('environmentId') != environment_id
                or grant['payload'].get('controllerOwner') != {'kind': 'maintenanceOperation', 'operationId': operation_id}
                or grant['payload'].get('controllerPurpose') != 'execute'):
            raise WorkspaceError('maintenance_grant_binding_mismatch')

    def _issue(self, owned, request, operation_id):
        observed = self._observe(owned.session_id, owned.environment_id)
        if (observed['instanceId'] != owned.instance_id or observed['containerId'] != owned.container_id
                or observed['hostId'] != owned.host_id or observed['profileDigest'] != owned.profile_digest):
            raise WorkspaceError('maintenance_instance_mismatch')
        grant = self.api.post(f'/internal/environments/{_segment(owned.environment_id)}/maintenance-authorizations',
            {'schema': 'workspace.environment_maintenance.issue.v1', 'operationId': operation_id,
                'controllerPurpose': 'execute', 'expectedControllerEpoch': observed['controllerEpoch'], 'request': request})
        self._check_grant(grant, owned.environment_id, operation_id, 'task')
        payload = grant['payload']
        if (payload.get('instanceId') != owned.instance_id or payload.get('containerId') != owned.container_id
                or payload.get('profileDigest') != owned.profile_digest or payload.get('requestDigest') != self._digest(request)):
            raise WorkspaceError('maintenance_grant_binding_mismatch')
        return grant

    def _release(self, grant):
        payload = grant['payload']
        operation = payload['controllerOwner']['operationId']
        result = self.api.post(f"/internal/environments/{_segment(payload['environmentId'])}/maintenance-operations/{_segment(operation)}/release",
            {'schema': 'workspace.environment_maintenance.release.v1', 'controllerEpoch': payload['controllerEpoch'],
                'controllerLeaseDigest': payload['controllerLeaseDigest']})
        expected = {'environmentId': payload['environmentId'], 'controllerOwner': payload['controllerOwner'],
            'controllerEpoch': payload['controllerEpoch'], 'controllerLeaseDigest': payload['controllerLeaseDigest'], 'released': True}
        if result != expected:
            raise WorkspaceError('maintenance_release_response_invalid')

    def _task(self, grant, command, request):
        value = self.runtime.post('/internal/environments/maintenance/task', {'authorization': grant,
            'request': command, 'task': {'schema': 'environment.task.v1', 'request': request}})
        if (not isinstance(value, dict) or set(value) != {'schema', 'status', 'result'}
                or value['schema'] != 'environment.task.v1' or value['status'] != 'success'
                or not isinstance(value['result'], dict)):
            raise WorkspaceError('maintenance_task_unconfirmed')
        return value['result']

    def exec(self, owned, *, command, cwd=None, env=None, timeout_sec=None, user=None):
        timeout_sec = 3600 if timeout_sec is None else timeout_sec
        if type(timeout_sec) not in {int, float} or not math.isfinite(timeout_sec) or not 0 < timeout_sec <= 3600:
            raise ValueError('maintenance exec timeout must be between zero and 3600 seconds')
        if not isinstance(command, str) or not command:
            raise ValueError('maintenance command must be nonempty text')
        profile = self._profiles.get(owned.environment_id)
        if profile is None:
            self._observe(owned.session_id, owned.environment_id)
            profile = self._profiles[owned.environment_id]
        if user is not None and str(user) not in {profile['user'], profile['user'].split(':')[0]}:
            raise WorkspaceError('maintenance_user_profile_mismatch')
        environment = env or {}
        if (not isinstance(environment, dict) or any(not isinstance(k, str) or not k or '=' in k or '\x00' in k
                or not isinstance(v, str) or '\x00' in v for k, v in environment.items())):
            raise ValueError('invalid maintenance environment')
        argv = ['/bin/bash', '-lc', command]
        if environment:
            argv = ['/usr/bin/env', '--', *[f'{k}={v}' for k, v in sorted(environment.items())], *argv]
        logical = {'program': argv[0], 'args': argv[1:], 'cwd': cwd or profile['workspaceRoot'],
            'timeoutMs': math.ceil(timeout_sec * 1000)}
        operation = 'maintenance-' + uuid.uuid4().hex
        grant = self._issue(owned, logical, operation)
        payload = grant['payload']
        fence = {'environmentInstanceId': owned.instance_id, 'controllerOwner': payload['controllerOwner'],
            'controllerEpoch': payload['controllerEpoch'], 'controllerPurpose': payload['controllerPurpose']}
        prefix = str(payload['controllerEpoch']) + ':'
        start_operation = prefix + 'execute'
        scope = {'kind': 'operation', 'operationId': operation}
        try:
            takeover = self._task(grant, logical, {'operation': 'takeover', **fence})
            if (set(takeover) != {'kind', 'fence'} or takeover['kind'] != 'takenOver'
                    or takeover['fence'] != fence or type(takeover['fence'].get('controllerEpoch')) is not int):
                raise WorkspaceError('maintenance_task_unconfirmed')
            result = self._task(grant, logical, {'operation': 'start', 'fence': fence, 'operationId': start_operation,
                'scope': scope, **logical})
            initial = _task_page(result, start_operation, scope)
            task_id = initial['taskId']
            cursor = 0
            streams = {'stdout': bytearray(), 'stderr': bytearray()}
            deadline = time.monotonic() + timeout_sec + 10
            while time.monotonic() < deadline:
                result = self._task(grant, logical, {'operation': 'query', 'fence': fence, 'taskId': task_id,
                    'cursor': cursor, 'maxBytes': TRANSFER_BLOCK_BYTES})
                snapshot = _task_page(result, initial['operationId'], initial['scope'], task_id=task_id, cursor=cursor)
                for chunk in result['output']:
                    streams[chunk['stream']].extend(chunk['data'])
                if sum(map(len, streams.values())) > 8 * 1024 * 1024:
                    raise WorkspaceError('maintenance_task_output_limit_exceeded')
                cursor = result['nextCursor']
                if (not result['hasMore'] and snapshot.get('state') == 'exited'
                        and snapshot.get('cleanupComplete') is True and snapshot.get('outputComplete') is True):
                    if type(snapshot.get('exitCode')) is not int:
                        raise WorkspaceError('maintenance_task_outcome_unknown')
                    return {'returnCode': snapshot['exitCode'], **{k: v.decode('utf-8', errors='replace') for k, v in streams.items()}}
                time.sleep(0.05)
            raise WorkspaceError('maintenance_task_timeout')
        finally:
            # EndOperation closes its journal before accepting any later Start;
            # this is safe even when Start's acknowledgement was lost.
            result = self._task(grant, logical, {'operation': 'endOperation', 'fence': fence,
                'ownerOperationId': operation, 'operationId': prefix + 'close'})
            if (set(result) != {'kind', 'taskIds', 'cleanupComplete'} or result['kind'] != 'operationEnded'
                    or result['cleanupComplete'] is not True or not isinstance(result['taskIds'], list)
                    or any(not _task_text(task_id, 128) for task_id in result['taskIds'])):
                raise WorkspaceError('maintenance_cleanup_unconfirmed')
            self._release(grant)

    def _filesystem(self, owned, path, operation, *, source=None, destination=None):
        profile = self._profiles.get(owned.environment_id)
        if profile is None:
            self._observe(owned.session_id, owned.environment_id)
            profile = self._profiles[owned.environment_id]
        operation_id = 'filesystem-' + uuid.uuid4().hex
        request = {'operationId': operation_id, 'cwd': profile['workspaceRoot'], 'modelPath': str(path),
            'policy': {'filesystem': {'workspaceRoot': profile['workspaceRoot'], 'readOnlyRoots': ['/'],
                'writableRoots': ['/'], 'deniedReadPaths': ['/run/centaeris', '/run/centaeris-host'],
                'deniedWritePaths': ['/run/centaeris', '/run/centaeris-host'], 'tmpRoot': None},
                'network': {'mode': profile['network']}}, 'operation': operation}
        length = Path(source).stat().st_size if source is not None else 0
        if length > EXPANDED_LIMIT:
            raise WorkspaceError('transfer_expanded_limit_exceeded')
        metadata = {'request': request, 'contentLength': length,
            'contentDigest': _content_digest(source) if source is not None else 'sha256:' + hashlib.sha256(b'').hexdigest()}
        grant = self._issue(owned, metadata, operation_id)
        try:
            result = self.runtime.filesystem({'authorization': grant, 'request': metadata}, source=source, destination=destination)
        finally:
            self._release(grant)
        if result['error'] is not None or result['output'] is None:
            raise WorkspaceError('maintenance_filesystem_failed')
        return result['output']

    def upload_file(self, owned, source, target):
        source = _safe_local(source)
        remote = _safe_name(str(PurePosixPath(target).relative_to('/')))
        if not source.is_file() or source.stat().st_size > EXPANDED_LIMIT:
            raise WorkspaceError('transfer_expanded_limit_exceeded')
        self._filesystem(owned, '/' + str(remote), {'type': 'writeFile', 'content': [],
            'observedFileHash': None, 'createOnly': False}, source=source)

    def upload_dir(self, owned, source, target):
        source = _safe_local(source)
        remote = _safe_name(str(PurePosixPath(target).relative_to('/')))
        entries, total = [], 0
        if not source.is_dir():
            raise WorkspaceError('transfer_source_directory_required')
        for current, directories, files in os.walk(source, followlinks=False):
            for name in sorted(directories + files):
                item = _safe_local(Path(current) / name)
                relative = _safe_name(item.relative_to(source).as_posix())
                if not item.is_dir() and not item.is_file():
                    raise WorkspaceError('transfer_archive_entry_rejected')
                total += item.stat().st_size if item.is_file() else 0
                entries.append((item, '/' + str(remote / relative)))
                if len(entries) > MEMBER_LIMIT or total > EXPANDED_LIMIT:
                    raise WorkspaceError('transfer_expanded_limit_exceeded')
        directories = ['/' + str(remote)]
        directories += [path for item, path in entries if item.is_dir()]
        self.exec(owned, command=shlex.join(['mkdir', '-p', '--', *directories]), timeout_sec=30)
        for item, path in entries:
            if item.is_file():
                self.upload_file(owned, item, path)
                self.exec(owned, command=shlex.join(['chmod', oct(item.stat().st_mode & 0o777)[2:], '--', path]), timeout_sec=30)

    def download_file(self, owned, source, target):
        remote = _safe_name(str(PurePosixPath(source).relative_to('/')))
        target = _safe_local(target)
        target.parent.mkdir(parents=True, exist_ok=True)
        descriptor, temporary = tempfile.mkstemp(prefix='.workspace-transfer-', dir=target.parent)
        os.close(descriptor)
        try:
            self._filesystem(owned, '/' + str(remote), {'type': 'readFile', 'maxBytes': EXPANDED_LIMIT}, destination=temporary)
            _safe_local(target)
            os.replace(temporary, target)
        finally:
            Path(temporary).unlink(missing_ok=True)

    def download_dir(self, owned, source, target):
        remote = _safe_name(str(PurePosixPath(source).relative_to('/')))
        target = _safe_local(target)
        output = self._filesystem(owned, '/' + str(remote), {'type': 'listDirectory', 'recursive': True, 'maxEntries': MEMBER_LIMIT})
        if not isinstance(output, dict) or set(output) != {'list_directory'}:
            raise WorkspaceError('transfer_response_invalid')
        listed = output['list_directory']
        if not isinstance(listed, dict) or not isinstance(listed.get('entries'), list):
            raise WorkspaceError('transfer_response_invalid')
        profile = self._profiles[owned.environment_id]
        source_root = PurePosixPath('/') / remote
        entries, total, names = [], 0, {}
        for entry in listed['entries']:
            if not isinstance(entry, dict) or not {'path', 'kind', 'sizeBytes'} <= set(entry):
                raise WorkspaceError('transfer_response_invalid')
            path = entry['path']
            # Core display paths are cwd-relative inside cwd, otherwise absolute.
            # Resolve that representation before stripping the requested root.
            clean = _safe_name(path.removeprefix('/') if isinstance(path, str) else path)
            absolute = PurePosixPath('/') / clean if path.startswith('/') else PurePosixPath(profile['workspaceRoot']) / clean
            try:
                relative = _safe_name(absolute.relative_to(source_root).as_posix())
            except ValueError:
                raise WorkspaceError('transfer_response_path_outside_root') from None
            size, kind = entry['sizeBytes'], entry['kind']
            if kind not in {'file', 'directory'} or kind == 'file' and (type(size) is not int or size < 0):
                raise WorkspaceError('transfer_response_invalid')
            key = relative.as_posix().casefold()
            if (key in names or any(names.get(parent.as_posix().casefold()) == 'file' for parent in relative.parents)
                    or kind == 'file' and any(name.startswith(key + '/') for name in names)):
                raise WorkspaceError('transfer_response_path_collision')
            names[key] = kind
            total += size if kind == 'file' else 0
            entries.append((relative, kind))
            _safe_local(target.joinpath(*relative.parts))
            if len(entries) > MEMBER_LIMIT or total > EXPANDED_LIMIT:
                raise WorkspaceError('transfer_expanded_limit_exceeded')
        target.mkdir(parents=True, exist_ok=True)
        for relative, kind in entries:
            destination = target.joinpath(*relative.parts)
            if kind == 'directory':
                destination.mkdir(parents=True, exist_ok=True)
            else:
                self.download_file(owned, '/' + str(remote / relative), destination)

    def retire(self, owned, operation_id):
        # Verifier maintenance outlives the model's execute controller. The
        # existing retire API requires the current epoch at first acceptance;
        # retain its exact request thereafter for immutable operation replay.
        for attempt in range(RETIREMENT_ATTEMPTS):
            request = self._retirement_requests.get(operation_id)
            if request is None:
                observed = self._observe(owned.session_id, owned.environment_id)
                if (observed["instanceId"] != owned.instance_id or observed["containerId"] != owned.container_id
                        or observed["hostId"] != owned.host_id or observed["profileDigest"] != owned.profile_digest
                        or observed["imageDigest"] != owned.image_digest
                        or type(observed["controllerEpoch"]) is not int or observed["controllerEpoch"] < 0):
                    raise WorkspaceError("retirement_binding_mismatch")
                if observed["state"] == "destroyed" and observed["physicalState"] == "absent" and observed["resourceHolding"] == 0:
                    return "destroyed"
                request = {"schema": "runtime.environment.retire.v1", "operationId": operation_id,
                    "environmentId": owned.environment_id, "instanceId": owned.instance_id, "hostId": owned.host_id,
                    "expectedControllerEpoch": observed["controllerEpoch"], "acknowledgeTmpfsLoss": True}
                self._retirement_requests[operation_id] = request
            elif (request["environmentId"] != owned.environment_id or request["instanceId"] != owned.instance_id
                    or request["hostId"] != owned.host_id):
                raise WorkspaceError("retirement_binding_mismatch")
            try:
                value = self.runtime.post("/internal/environments/retire", request)
            except WorkspaceError as error:
                if (error.status != 409 or error.code != "environment_retirement_epoch_conflict"
                        or operation_id in self._accepted_retirements):
                    raise
                # Only this response proves no retirement operation was
                # accepted. Reuse its ID with a newly observed epoch; transport
                # uncertainty and every other rejection keep the original body.
                del self._retirement_requests[operation_id]
                if attempt + 1 == RETIREMENT_ATTEMPTS:
                    raise
                continue
            _fields(value, "runtime.environment.retire.result.v1", {"operationId", "environmentId", "instanceId", "status"})
            if (value["operationId"] != operation_id or value["environmentId"] != owned.environment_id
                    or value["instanceId"] != owned.instance_id or value["status"] not in {"pending", "destroyed"}):
                raise WorkspaceError("retirement_response_invalid")
            self._accepted_retirements.add(operation_id)
            return value["status"]

    def observe_cleanup(self, owned):
        value = self._observe(owned.session_id, owned.environment_id)
        if value["instanceId"] not in {None, owned.instance_id} or value["containerId"] not in {None, owned.container_id}:
            raise WorkspaceError("cleanup_instance_mismatch")
        if value["physicalState"] == "absent" and value["state"] != "destroyed":
            # A raw missing container is insufficient until ledger retirement
            # has also reached its confirmed terminal state.
            return CleanupObservation(owned.environment_id, owned.instance_id, "unknown", value["resourceHolding"])
        return CleanupObservation(owned.environment_id, owned.instance_id, value["physicalState"], value["resourceHolding"])
