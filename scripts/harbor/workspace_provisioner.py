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
import tarfile
import tempfile
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

    def upload_archive(self, path, metadata, archive):
        header = json.dumps(metadata, ensure_ascii=True, separators=(",", ":"))
        if len(header) > _TRANSFER_CONTRACT["metadataBytes"]:
            raise WorkspaceError("transfer_metadata_limit_exceeded")
        with Path(archive).open("rb") as source:
            request = self._request("POST", path, headers={"Content-Type": "application/x-tar",
                "X-Benchmark-Transfer": header})
            request.data = iter(lambda: source.read(TRANSFER_BLOCK_BYTES), b"")
            request.add_header("Content-Length", str(os.fstat(source.fileno()).st_size))
            return self._send(request)

    def download_archive(self, path, value, archive):
        request = self._request("POST", path, value, headers={"Accept": "application/x-tar"})
        return self._send(request, archive=archive)

    def _send(self, request, *, timeout=None, archive=None):
        request.add_header("X-Environment-Maintenance-Token", self.__maintenance_token)
        if self.__internal_token is not None:
            request.add_header("X-Internal-Token", self.__internal_token)
        try:
            with self.opener.open(request, timeout=timeout or self.request_timeout) as response:
                if archive is not None:
                    if response.headers.get("Content-Type") != "application/x-tar":
                        raise WorkspaceError("transfer_response_invalid")
                    declared = response.headers.get("Content-Length")
                    if declared is not None:
                        if not declared.isdecimal():
                            raise WorkspaceError("transfer_response_invalid")
                        declared = int(declared)
                        if declared > ARCHIVE_LIMIT:
                            raise WorkspaceError("transfer_archive_limit_exceeded")
                    with Path(archive).open("wb") as output:
                        bounded = _LimitedWriter(output, ARCHIVE_LIMIT)
                        while block := response.read(TRANSFER_BLOCK_BYTES):
                            bounded.write(block)
                        if declared is not None and bounded.written != declared:
                            raise WorkspaceError("transfer_response_incomplete")
                    return
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

    @staticmethod
    def _binding(owned):
        if not isinstance(owned, OwnedEnvironment):
            raise WorkspaceError("owned_environment_required")
        return {"sessionId": owned.session_id, "environmentId": owned.environment_id,
                "agentRunId": owned.agent_run_id, "instanceId": owned.instance_id,
                "profileDigest": owned.profile_digest}

    def bind_profile(self, session_id, environment_id, profile, operation_id):
        request = {"schema": "workspace.benchmark_profile.bind.v1", "operationId": operation_id,
                   "sessionId": session_id, "profile": profile}
        path = f"/internal/environments/{_segment(environment_id)}/benchmark-profile"
        # Only immutable metadata with the same operation and body may be
        # repeated. A dropped exec or transfer is never retried this way.
        try:
            value = self.api.post(path, request)
        except WorkspaceError as error:
            if error.code != "transport_unavailable":
                raise
            value = self.api.post(path, request)
        _fields(value, "workspace.benchmark_profile.bound.v1",
                {"operationId", "sessionId", "environmentId", "profileDigest", "profile"})
        expected_digest = "sha256:" + hashlib.sha256(json.dumps(profile, sort_keys=True, ensure_ascii=False,
            separators=(",", ":"), allow_nan=False).encode()).hexdigest()
        if (value["operationId"] != operation_id or value["sessionId"] != session_id
                or value["environmentId"] != environment_id or value["profileDigest"] != expected_digest
                or value["profile"] != profile):
            raise WorkspaceError("benchmark_profile_binding_mismatch")
        return value

    def _observe(self, session_id, environment_id, agent_run_id):
        request = {"schema": "runtime.benchmark.observe.v1", "sessionId": session_id,
                   "environmentId": environment_id, "agentRunId": agent_run_id}
        value = self.runtime.post("/internal/benchmark/observe", request)
        _fields(value, "runtime.benchmark.observe.result.v1", {"state", "sessionId", "environmentId", "agentRunId",
            "instanceId", "containerId", "hostId", "controllerEpoch", "imageDigest", "profileDigest",
            "resourceHolding", "physicalState"})
        if (any(value[key] != request[key] for key in ("sessionId", "environmentId", "agentRunId"))
                or value["state"] not in {"allocationPending", "waitingSetup", "ready", "destroyed", "unknown"}
                or value["physicalState"] not in {"present", "absent", "unknown"}
                or (value["resourceHolding"] is not None and (type(value["resourceHolding"]) is not int
                    or value["resourceHolding"] not in {0, 1}))):
            raise WorkspaceError("benchmark_observation_invalid")
        return value

    def observe_setup(self, session_id, environment_id, agent_run_id):
        try:
            value = self._observe(session_id, environment_id, agent_run_id)
        except WorkspaceError as error:
            # Setup's existing deadline controls re-observation. Only the
            # explicit read-only store-unavailable response is pending; never
            # reinterpret an identity conflict or uncertain mutation as retryable.
            if error.status == 503 and error.code == "benchmark_observation_unavailable":
                return None
            raise
        if value["state"] in {"allocationPending", "unknown"}:
            return None
        if (value["state"] == "waitingSetup" and value["physicalState"] == "unknown"
                and value["resourceHolding"] == 0
                and all(value[key] is None for key in ("instanceId", "containerId", "hostId"))):
            return None
        if value["state"] == "destroyed":
            raise WorkspaceError("environment_destroyed")
        if value["physicalState"] != "present" or value["resourceHolding"] != 1:
            raise WorkspaceError("benchmark_observation_invalid")
        try:
            return OwnedEnvironment(session_id, environment_id, agent_run_id, value["instanceId"],
                value["containerId"], value["hostId"], value["controllerEpoch"], value["imageDigest"],
                value["profileDigest"], value["state"], value["resourceHolding"])
        except ValueError:
            raise WorkspaceError("benchmark_observation_invalid") from None

    def activate_setup(self, owned, operation_id):
        request = {"schema": "workspace.benchmark_setup.complete.v1", "operationId": operation_id,
            "sessionId": owned.session_id, "agentRunId": owned.agent_run_id, "instanceId": owned.instance_id,
            "containerId": owned.container_id}
        path = f"/internal/environments/{_segment(owned.environment_id)}/benchmark-setup-complete"
        try:
            value = self.api.post(path, request)
        except WorkspaceError as error:
            if error.code != "transport_unavailable":
                raise
            observed = self._observe(owned.session_id, owned.environment_id, owned.agent_run_id)
            if observed["state"] == "ready" and observed["instanceId"] == owned.instance_id and observed["containerId"] == owned.container_id:
                return "ready"
            raise WorkspaceError("benchmark_activation_uncertain") from None
        _fields(value, "workspace.benchmark_setup.completed.v1",
                {"operationId", "sessionId", "environmentId", "agentRunId", "instanceId", "containerId"})
        if (any(value[key] != item for key, item in request.items() if key != "schema")
                or value["environmentId"] != owned.environment_id):
            raise WorkspaceError("benchmark_activation_binding_mismatch")
        return "ready"

    def exec(self, owned, *, command, cwd=None, env=None, timeout_sec=None, user=None):
        timeout_sec = 3600 if timeout_sec is None else timeout_sec
        if type(timeout_sec) not in {int, float} or not math.isfinite(timeout_sec) or not 0 < timeout_sec <= 3600:
            raise ValueError("maintenance exec timeout must be between zero and 3600 seconds")
        request = {"schema": "runtime.benchmark.exec.v1", **self._binding(owned),
            "command": ["/bin/bash", "-lc", command], "cwd": cwd, "env": env or {},
            "user": str(user) if user is not None else None, "timeoutMs": math.ceil(timeout_sec * 1000)}
        value = self.runtime.post("/internal/benchmark/exec", request, timeout=timeout_sec + 10)
        _fields(value, "runtime.benchmark.exec.result.v1", {"exitCode", "stdout", "stderr", "timedOut", "outcome"})
        if (value["outcome"] not in {"completed", "unknown"} or type(value["timedOut"]) is not bool
                or any(not isinstance(value[key], str) for key in ("stdout", "stderr"))):
            raise WorkspaceError("benchmark_exec_response_invalid")
        if value["outcome"] == "unknown":
            raise WorkspaceError("benchmark_exec_outcome_unknown")
        if value["timedOut"]:
            raise WorkspaceError("benchmark_exec_timeout")
        if type(value["exitCode"]) is not int:
            raise WorkspaceError("benchmark_exec_response_invalid")
        return {"returnCode": value["exitCode"], "stdout": value["stdout"], "stderr": value["stderr"]}

    def _upload(self, owned, source, target, *, directory):
        remote = PurePosixPath(target)
        if not remote.is_absolute() or ".." in remote.parts or "\\" in target or "\x00" in target:
            raise ValueError("upload target must be an absolute task path")
        destination = str(remote if directory else remote.parent)
        with tempfile.TemporaryDirectory(prefix="workspace-upload-") as temporary:
            archive = Path(temporary) / "payload.tar"
            pack_upload(source, archive, directory=directory, archive_name=remote.name)
            # Dispatch each mutation once; uncertainty never replays an upload.
            self.exec(owned, command=shlex.join(["mkdir", "-p", "--", destination]), cwd="/",
                      env={}, timeout_sec=30, user="0")
            value = self.runtime.upload_archive("/internal/benchmark/upload", {"schema": "runtime.benchmark.upload.v1",
                **self._binding(owned), "path": destination}, archive)
            _fields(value, "runtime.benchmark.upload.result.v1", {"uploadedBytes"})
            if type(value["uploadedBytes"]) is not int or value["uploadedBytes"] != archive.stat().st_size:
                raise WorkspaceError("benchmark_upload_response_invalid")

    def upload_file(self, owned, source, target):
        self._upload(owned, source, target, directory=False)

    def upload_dir(self, owned, source, target):
        self._upload(owned, source, target, directory=True)

    def _download(self, owned, source, target, *, directory):
        remote = PurePosixPath(source)
        if not remote.is_absolute() or ".." in remote.parts or "\\" in source or "\x00" in source:
            raise ValueError("download source must be an absolute task path")
        with tempfile.TemporaryDirectory(prefix="workspace-download-") as temporary:
            archive = Path(temporary) / "payload.tar"
            self.runtime.download_archive("/internal/benchmark/download", {"schema": "runtime.benchmark.download.v1",
                **self._binding(owned), "path": source}, archive)
            unpack_download(archive, target, directory=directory, strip_root=remote.name if directory else None)

    def download_file(self, owned, source, target):
        self._download(owned, source, target, directory=False)

    def download_dir(self, owned, source, target):
        self._download(owned, source, target, directory=True)

    def retire(self, owned, operation_id):
        # Verifier maintenance outlives the model's execute controller. The
        # existing retire API requires the current epoch at first acceptance;
        # retain its exact request thereafter for immutable operation replay.
        request = self._retirement_requests.get(operation_id)
        if request is None:
            observed = self._observe(owned.session_id, owned.environment_id, owned.agent_run_id)
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
        value = self.runtime.post("/internal/environments/retire", request)
        _fields(value, "runtime.environment.retire.result.v1", {"operationId", "environmentId", "instanceId", "status"})
        if (value["operationId"] != operation_id or value["environmentId"] != owned.environment_id
                or value["instanceId"] != owned.instance_id or value["status"] not in {"pending", "destroyed"}):
            raise WorkspaceError("retirement_response_invalid")
        return value["status"]

    def observe_cleanup(self, owned):
        value = self._observe(owned.session_id, owned.environment_id, owned.agent_run_id)
        if value["instanceId"] not in {None, owned.instance_id} or value["containerId"] not in {None, owned.container_id}:
            raise WorkspaceError("cleanup_instance_mismatch")
        if value["physicalState"] == "absent" and value["state"] != "destroyed":
            # A raw missing container is insufficient until ledger retirement
            # has also reached its confirmed terminal state.
            return CleanupObservation(owned.environment_id, owned.instance_id, "unknown", value["resourceHolding"])
        return CleanupObservation(owned.environment_id, owned.instance_id, value["physicalState"], value["resourceHolding"])
