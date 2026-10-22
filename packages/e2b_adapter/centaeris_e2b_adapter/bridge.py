"""Bounded SDK translation; durable authority remains with the Runtime caller."""

import base64
import binascii
from collections import OrderedDict
from dataclasses import dataclass, field
import importlib.metadata
import json
import os
import re
from time import monotonic
from typing import BinaryIO
from urllib.parse import urlsplit
from uuid import UUID

from .attach import attach_existing
from .bulk import BulkSpool, BulkTransferError, MAX_TRANSFER_BYTES, TRANSFER_CHUNK_BYTES
from .control import (CONTROL_HELPERS, EXPECTED_METADATA_KEYS, WORKSPACE_ROOT, WORKSPACE_USER,
                      ControlHelperError, start_control_helper)

SDK_VERSION = "2.58.0"
REQUEST_SCHEMA = "runtime.e2b.adapter.request.v1"
RESPONSE_SCHEMA = "runtime.e2b.adapter.response.v1"
MAX_FRAME_BYTES = 1024 * 1024
MAX_FILE_BYTES = 256 * 1024
MAX_OUTPUT_BYTES = 64 * 1024
MAX_CONNECTED_SANDBOXES = 128
MAX_COMMANDS = 128


class RequestError(ValueError):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


@dataclass(frozen=True)
class _ConnectedSandbox:
    sandbox: object
    envd_version: str


@dataclass(frozen=True)
class _CommandEnd:
    exit_code: int
    exited: bool


class _RawCommandHandle:
    def __init__(self, pid, events):
        self.pid = pid
        self.events = events

    def __iter__(self):
        from protobuf import Oneof

        for response in self.events:
            match response.event.event if response.event is not None else None:
                case Oneof(field="data", value=data):
                    match data.output:
                        case Oneof(field="stdout", value=chunk):
                            yield chunk, None, None
                        case Oneof(field="stderr", value=chunk):
                            yield None, chunk, None
                        case _:
                            raise RequestError("commandStreamInvalid", "Unsupported command output event")
                case Oneof(field="end", value=end):
                    # A normal nonzero exit carries provider error text too.
                    # The explicit exit evidence is useful; its diagnostics are
                    # not returned or retained in the adapter receipt.
                    yield None, None, _CommandEnd(end.exit_code, end.exited)
                case Oneof(field="keepalive"):
                    continue
                case _:
                    raise RequestError("commandStreamInvalid", "Unsupported command stream event")

    def disconnect(self):
        self.events.close()


def _open_process_stream(url, connection, envd_version, user, process, timeout_seconds):
    # This is the generated protocol shipped by the exact pinned official SDK.
    # Its public CommandHandle decodes bytes before exposing output callbacks.
    from e2b.connection_config import KEEPALIVE_PING_HEADER, KEEPALIVE_PING_INTERVAL_SEC
    from e2b.envd.client_sync import as_stream, create_rpc_client
    from e2b.envd.process import process_connect, process_pb
    from e2b.envd.utils import authentication_header, extract_start_pid, timeout_to_ms
    from packaging.version import Version

    rpc = create_rpc_client(process_connect.ProcessClientSync, url, connection)
    events = as_stream(rpc.start(
        process_pb.StartRequest(process=process, stdin=False),
        headers={**authentication_header(Version(envd_version), user),
                 KEEPALIVE_PING_HEADER: str(KEEPALIVE_PING_INTERVAL_SEC)},
        timeout_ms=timeout_to_ms(timeout_seconds)))
    try:
        pid = extract_start_pid(next(events), "start process")
        if type(pid) is not int or not 1 <= pid <= 2**31 - 1:
            raise RequestError("commandStreamInvalid", "Provider returned an invalid process identity")
        return _RawCommandHandle(pid, events)
    except BaseException:
        events.close()
        raise



def _start_command_stream(sandbox, envd_version, payload):
    from e2b.envd.process import process_pb

    process = process_pb.ProcessConfig(cmd="/bin/bash", args=["-l", "-c", payload["command"]],
                                      cwd=payload.get("cwd"), envs=payload.get("envs"))
    return _open_process_stream(sandbox.envd_api_url, sandbox.connection_config, envd_version,
                                payload.get("user"), process, payload["timeoutSeconds"])


@dataclass
class _CommandEvidence:
    handle: object | None
    deadline: float
    result: dict | None = None


@dataclass(frozen=True)
class AdapterConfig:
    api_key: str = field(repr=False)
    domain: str | None = None
    api_url: str | None = None
    sandbox_url: str | None = None
    spool_root: str | None = field(default=None, repr=False)
    idempotency_supported: bool = False
    lane: str = "ordinary"
    request_timeout_seconds: float = 30

    def __post_init__(self):
        if not self.api_key:
            raise ValueError("Explicit API key is required")
        if self.domain is not None and (not self.domain or any(c in self.domain for c in "/?#@")):
            raise ValueError("Invalid provider domain")
        if (self.api_url is None) != (self.sandbox_url is None):
            raise ValueError("Provider API and sandbox URLs must be configured together")
        if self.domain is None and self.api_url is None:
            raise ValueError("Explicit provider domain or paired endpoint URLs are required")
        if self.lane not in ("ordinary", "control", "cleanup"):
            raise ValueError("Unsupported adapter lane")
        if not 0 < self.request_timeout_seconds <= 60:
            raise ValueError("Request timeout must be between zero and 60 seconds")
        for endpoint in (self.api_url, self.sandbox_url):
            if endpoint is not None:
                _validate_endpoint_url(endpoint)


def _validate_endpoint_url(value):
    try:
        if not isinstance(value, str) or any(ord(c) < 32 or ord(c) == 127 for c in value):
            raise ValueError()
        url = urlsplit(value)
        if url.username or url.password or url.query or url.fragment or not url.hostname:
            raise ValueError()
        if url.port is not None and not 1 <= url.port <= 65535:
            raise ValueError()
        if url.scheme != "https" and not (url.scheme == "http" and url.hostname in ("localhost", "127.0.0.1", "::1")):
            raise ValueError()
    except ValueError:
        raise ValueError("Provider endpoints require credential-free HTTPS URLs or explicit HTTP loopback URLs") from None


def _object(value, required, optional=()):
    if not isinstance(value, dict) or set(value) - set(required) - set(optional) or set(required) - set(value):
        raise RequestError("invalidRequest", "Missing or unsupported request fields")
    return dict(value)


def _text(value, max_bytes=4096):
    if not isinstance(value, str) or not value or "\x00" in value or len(value.encode("utf-8")) > max_bytes:
        raise RequestError("invalidRequest", "Invalid bounded string")
    return value


def _integer(value, minimum=1, maximum=86400):
    if type(value) is not int or not minimum <= value <= maximum:
        raise RequestError("invalidRequest", "Integer is outside the supported range")
    return value


def _choice(value, options):
    if value not in options or not isinstance(value, str):
        raise RequestError("invalidRequest", "Unsupported policy value")
    return value


def _strings(value):
    if not isinstance(value, dict) or len(value) > 64:
        raise RequestError("invalidRequest", "Expected a bounded string map")
    return {_text(k, 128): _text(v, 4096) for k, v in value.items()}


class Adapter:
    def __init__(self, config: AdapterConfig, *, client=None, command_stream_factory=None):
        self.config = config
        self._sandboxes = OrderedDict()
        self._commands = OrderedDict()
        self._spool = BulkSpool(config.spool_root) if config.spool_root is not None else None
        self._command_stream_factory = command_stream_factory or _start_command_stream
        if client is None:
            if ((config.api_url is None and os.environ.get("E2B_API_URL"))
                    or (config.sandbox_url is None and os.environ.get("E2B_SANDBOX_URL"))):
                raise ValueError("Provider endpoint overrides must be selected explicitly in adapter configuration")
            # Each lane is a dedicated process, including its SDK connection pool.
            # A connection failure cannot silently multiply a caller's dispatches.
            os.environ["E2B_CONNECTION_RETRIES"] = "0"
            if importlib.metadata.version("e2b") != SDK_VERSION:
                raise ValueError("Unsupported E2B SDK version")
            from e2b import E2B

            client = E2B(api_key=config.api_key, domain=config.domain,
                         api_url=config.api_url, sandbox_url=config.sandbox_url, retries=0, debug=False,
                         request_timeout=config.request_timeout_seconds)
        self.client = client

    def handle(self, request):
        request_id = None
        known = {}
        transfer = None
        try:
            value = _object(request, ("schema", "requestId", "operation", "payload"))
            request_id = _text(value["requestId"], 128)
            if value["schema"] != REQUEST_SCHEMA:
                raise RequestError("invalidRequest", "Unsupported adapter schema")
            operation = _text(value["operation"], 64)
            payload = self._validate(operation, value["payload"])
            self._check_lane(operation)
            if operation in ("readFile", "writeFile", "uploadFile", "downloadFile", "runCommand", "waitCommand", "recoverCommand", "cancelCommand"):
                if payload["sandboxId"] not in self._sandboxes:
                    raise RequestError("connectionRequired", "Explicit connect is required in this adapter process")
            known = {k: payload[k] for k in ("attemptId", "sandboxId", "pid", "transferId") if k in payload}
            if operation in ("uploadFile", "downloadFile"):
                if self._spool is None:
                    raise RequestError("bulkUnavailable", "Bulk file transfer requires an explicitly configured private host spool")
                transfer = self._spool.prepare(operation, payload)
            if operation == "runCommand":
                self._admit_command()
            if operation in ("waitCommand", "recoverCommand"):
                evidence = self._commands.get((payload["sandboxId"], payload["pid"]))
                if evidence is not None and evidence.handle is not None:
                    if payload["timeoutSeconds"] < evidence.deadline - monotonic():
                        raise RequestError("waitBudgetInsufficient", "Wait budget must cover the original command stream's remaining deadline")
        except (RequestError, BulkTransferError, OSError, UnicodeError, TypeError) as error:
            return self._response(request_id, "notDispatched", **({"result": known} if known else {}), error={"code": getattr(error, "code", "invalidRequest"), "message": str(error) if isinstance(error, (RequestError, BulkTransferError)) else "Invalid request or unavailable private spool"})
        try:
            result = self._execute(operation, payload, known, transfer)
            if transfer is not None:
                transfer.close()
                transfer = None
            return self._response(request_id, "succeeded", result=result)
        except (RequestError, BulkTransferError, ControlHelperError) as error:
            return self._response(request_id, "unknown", result=known, error={"code": error.code, "message": str(error)})
        except Exception:
            # Provider diagnostics may contain credentials, URLs or user content.
            return self._response(request_id, "unknown", result=known, error={"code": "providerRequestFailed", "message": "The provider result is unknown; retain the operation responsibility."})
        finally:
            if transfer is not None:
                try:
                    transfer.close()
                except OSError:
                    # Its trusted host owner still has the transfer identity.
                    pass

    @staticmethod
    def _response(request_id, outcome, **extra):
        return {"schema": RESPONSE_SCHEMA, "requestId": request_id, "outcome": outcome, **extra}

    def _check_lane(self, operation):
        allowed = {
            "ordinary": {"capabilities", "create", "connect", "attach", "inspect", "readFile", "writeFile", "uploadFile", "downloadFile", "runCommand", "waitCommand", "recoverCommand", "cancelCommand"},
            "control": {"capabilities", "inspect", "invokeControlHelper"},
            "cleanup": {"capabilities", "inspect", "kill", "pause"},
        }
        if operation not in allowed[self.config.lane]:
            raise RequestError("laneForbidden", "Operation is not permitted in this adapter lane")

    def _validate(self, operation, payload):
        specs = {
            "capabilities": ((), ()),
            "create": (("attemptId", "idempotencyKey", "templateRef", "metadata", "timeoutSeconds", "allowInternetAccess", "onTimeout"), ()),
            "connect": (("sandboxId", "timeoutSeconds", "onResume"), ()),
            "inspect": (("sandboxId",), ()),
            "attach": (("sandboxId", "expectedTemplateId", "expectedMetadata", "workspaceRoot", "workspaceUser"), ()),
            "kill": (("sandboxId",), ()),
            "pause": (("sandboxId", "mode"), ()),
            "readFile": (("sandboxId", "path"), ("user", "maxBytes")),
            "writeFile": (("sandboxId", "path", "dataBase64"), ("user",)),
            "uploadFile": (("sandboxId", "path", "transferId", "byteLength", "sha256"), ("user",)),
            "downloadFile": (("sandboxId", "path", "transferId", "maxBytes"), ("user",)),
            "runCommand": (("sandboxId", "command", "timeoutSeconds"), ("user", "cwd", "envs")),
            "waitCommand": (("sandboxId", "pid", "timeoutSeconds"), ("maxOutputBytes",)),
            "recoverCommand": (("sandboxId", "pid", "timeoutSeconds"), ("maxOutputBytes",)),
            "cancelCommand": (("sandboxId", "pid"), ()),
            "invokeControlHelper": (("sandboxId", "helper", "expectedTemplateId", "expectedMetadata", "workspaceRoot", "workspaceUser", "timeoutSeconds", "maxOutputBytes"), ()),
        }
        if operation not in specs:
            raise RequestError("unsupportedOperation", "Unsupported adapter operation")
        value = _object(payload, *specs[operation])
        for key in ("sandboxId", "attemptId", "user"):
            if key in value:
                _text(value[key], 256)
        for key in ("path", "cwd"):
            if key in value and not _text(value[key]).startswith("/"):
                raise RequestError("invalidRequest", "Sandbox paths must be absolute")
        if "command" in value:
            _text(value["command"], MAX_OUTPUT_BYTES)
        if "timeoutSeconds" in value:
            _integer(value["timeoutSeconds"])
        if "pid" in value:
            _integer(value["pid"], 1, 2**31 - 1)
        for key, maximum in (("maxBytes", MAX_TRANSFER_BYTES if operation == "downloadFile" else MAX_FILE_BYTES), ("maxOutputBytes", MAX_OUTPUT_BYTES)):
            if key in value:
                _integer(value[key], 1, maximum)
        if "envs" in value:
            value["envs"] = _strings(value["envs"])
        if operation in ("uploadFile", "downloadFile"):
            transfer_id = _text(value["transferId"], 36)
            try:
                if str(UUID(transfer_id)) != transfer_id:
                    raise ValueError()
            except ValueError:
                raise RequestError("invalidRequest", "Transfer identity requires a canonical lowercase UUID") from None
            if operation == "uploadFile":
                _integer(value["byteLength"], 0, MAX_TRANSFER_BYTES)
                if not re.fullmatch(r"[a-f0-9]{64}", _text(value["sha256"], 64)):
                    raise RequestError("invalidRequest", "Upload requires an exact lowercase SHA-256 digest")
        if operation == "invokeControlHelper":
            _choice(value["helper"], CONTROL_HELPERS)
            _integer(value["timeoutSeconds"], 1, 60)
        if operation in ("invokeControlHelper", "attach"):
            if value["workspaceRoot"] != WORKSPACE_ROOT or value["workspaceUser"] != WORKSPACE_USER:
                raise RequestError("invalidRequest", "Fixed control helpers require the supported template workspace identity")
            if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,127}", _text(value["expectedTemplateId"], 128)):
                raise RequestError("invalidRequest", "Fixed control helpers require a canonical expected template ID")
            value["expectedMetadata"] = _strings(_object(value["expectedMetadata"], EXPECTED_METADATA_KEYS))
        if operation == "create":
            if not re.fullmatch(r"[a-z0-9-]+", _text(value["idempotencyKey"], 255)):
                raise RequestError("invalidRequest", "Create idempotencyKey requires 1 to 255 lowercase ASCII letters, digits or hyphens")
            ref = _object(value["templateRef"], ("templateId", "buildId"))
            _text(ref["templateId"], 128)
            _text(ref["buildId"], 36)
            try:
                if (not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,127}", ref["templateId"])
                        or str(UUID(ref["buildId"])) != ref["buildId"]):
                    raise ValueError()
            except ValueError:
                raise RequestError("invalidRequest", "Template requires a canonical ID and immutable lowercase build UUID") from None
            value["templateRef"] = ref
            value["metadata"] = _strings(value["metadata"])
            if "centaerisAttemptId" in value["metadata"] and value["metadata"]["centaerisAttemptId"] != value["attemptId"]:
                raise RequestError("invalidRequest", "Conflicting create attempt metadata")
            value["metadata"]["centaerisAttemptId"] = value["attemptId"]
            if type(value["allowInternetAccess"]) is not bool:
                raise RequestError("invalidRequest", "Internet access must be explicit")
            _choice(value["onTimeout"], ("kill", "pause"))
        if operation == "connect":
            _choice(value["onResume"], ("restore",))
        if operation == "pause":
            _choice(value["mode"], ("full", "filesystem"))
        if operation == "writeFile":
            try:
                encoded = value["dataBase64"]
                if encoded != "":
                    _text(encoded, (MAX_FILE_BYTES + 2) // 3 * 4)
                value["data"] = base64.b64decode(encoded, validate=True)
                if len(value["data"]) > MAX_FILE_BYTES:
                    raise ValueError()
            except (ValueError, binascii.Error):
                raise RequestError("invalidRequest", "Invalid or oversized base64 file content") from None
        return value

    def _remember(self, sandbox, envd_version):
        self._sandboxes[sandbox.sandbox_id] = _ConnectedSandbox(sandbox, envd_version)
        self._sandboxes.move_to_end(sandbox.sandbox_id)
        if len(self._sandboxes) > MAX_CONNECTED_SANDBOXES:
            self._sandboxes.popitem(last=False)

    def _admit_command(self):
        if len(self._commands) < MAX_COMMANDS:
            return
        completed = next((key for key, evidence in self._commands.items() if evidence.result is not None), None)
        if completed is None:
            raise RequestError("commandCapacityUnavailable", "All bounded command evidence slots hold pending streams")
        self._commands.pop(completed)

    @staticmethod
    def _info(info, expected_sandbox_id):
        if info.sandbox_id != expected_sandbox_id:
            raise RequestError("identityMismatch", "Provider returned a different sandbox identity")
        state = getattr(info.state, "value", info.state)
        return {"sandboxId": info.sandbox_id, "templateId": info.template_id, "metadata": info.metadata, "state": state}

    def _execute(self, operation, payload, known, transfer=None):
        if operation == "capabilities":
            return {"sdkVersion": SDK_VERSION, "lane": self.config.lane, "createIdempotencyConfigured": self.config.idempotency_supported,
                    "readOnlyAttach": True,
                    "commandStartIdempotency": False, "runProcessTreeCancellation": False, "controlCommandCancellation": False,
                    "commandOutputBytes": True, "commandOutputRestartRecovery": False, "commandOutputRecovery": "adapterLifetime",
                    "waitBudgetMustCoverOriginalStream": True, "maxCommandEvidence": MAX_COMMANDS,
                    "fixedControlHelpers": list(CONTROL_HELPERS) if self.config.lane == "control" else [],
                    "bulkFileTransfer": self._spool is not None, "maxTransferBytes": MAX_TRANSFER_BYTES, "transferChunkBytes": TRANSFER_CHUNK_BYTES,
                    "storageQuota": False, "fullMemoryPauseVerified": False,
                    "maxFileBytes": MAX_FILE_BYTES, "maxOutputBytes": MAX_OUTPUT_BYTES}
        if operation == "invokeControlHelper":
            known["helper"] = payload["helper"]
            handle = start_control_helper(self.config, payload, _open_process_stream)
            known["pid"] = handle.pid
            return {"helper": payload["helper"], **_collect_command_output(handle, payload["sandboxId"], handle.pid, payload["maxOutputBytes"])}
        if operation == "attach":
            sandbox, detail = attach_existing(self.config, payload)
            self._remember(sandbox, detail.envd_version)
            return self._info(detail, payload["sandboxId"])
        sdk = self.client.Sandbox
        if operation == "create":
            # The caller durably admits one fresh dispatch before invoking us.
            # Configured provider idempotency neither grants dispatch authority
            # nor permits replay of an unresolved request.
            ref = payload["templateRef"]
            sandbox = sdk.create(template=f"{ref['templateId']}:{ref['buildId']}", timeout=payload["timeoutSeconds"],
                                 metadata=payload["metadata"], allow_internet_access=payload["allowInternetAccess"],
                                 lifecycle={"on_timeout": payload["onTimeout"], "auto_resume": False},
                                 api_headers={"Idempotency-Key": payload["idempotencyKey"]}, retries=0)
            known["sandboxId"] = sandbox.sandbox_id
            provider_info = sandbox.get_info()
            info = self._info(provider_info, sandbox.sandbox_id)
            self._remember(sandbox, provider_info.envd_version)
            return {**info, "attemptId": payload["attemptId"]}
        sandbox_id = payload["sandboxId"]
        if operation == "connect":
            sandbox = sdk.connect(sandbox_id, timeout=payload["timeoutSeconds"], on_resume=payload["onResume"])
            if sandbox.sandbox_id != sandbox_id:
                raise RequestError("identityMismatch", "Provider returned a different sandbox identity")
            provider_info = sandbox.get_info()
            info = self._info(provider_info, sandbox_id)
            self._remember(sandbox, provider_info.envd_version)
            return info
        if operation == "inspect":
            return self._info(sdk.get_info(sandbox_id), sandbox_id)
        if operation == "kill":
            changed = sdk.kill(sandbox_id)
            self._sandboxes.pop(sandbox_id, None)
            return {"sandboxId": sandbox_id, "deleteAcknowledged": True, "providerReportedKilled": changed}
        if operation == "pause":
            changed = sdk.pause(sandbox_id, mode=payload["mode"])
            self._sandboxes.pop(sandbox_id, None)
            return {"sandboxId": sandbox_id, "requestedMode": payload["mode"], "changed": changed}
        connection = self._sandboxes[sandbox_id]
        sandbox = connection.sandbox
        user = {"user": payload["user"]} if "user" in payload else {}
        if operation in ("uploadFile", "downloadFile"):
            return transfer.execute(sandbox, self.config.request_timeout_seconds)
        if operation == "readFile":
            data = bytearray()
            with sandbox.files.read(payload["path"], format="stream", request_timeout=self.config.request_timeout_seconds, **user) as stream:
                for chunk in stream:
                    if len(data) + len(chunk) > payload.get("maxBytes", MAX_FILE_BYTES):
                        raise RequestError("fileLimitExceeded", "File exceeds the bounded transfer size")
                    data.extend(chunk)
            return {"sandboxId": sandbox_id, "dataBase64": base64.b64encode(data).decode("ascii")}
        if operation == "writeFile":
            sandbox.files.write(payload["path"], payload["data"], request_timeout=self.config.request_timeout_seconds, **user)
            return {"sandboxId": sandbox_id, "bytesWritten": len(payload["data"])}
        if operation == "runCommand":
            handle = self._command_stream_factory(sandbox, connection.envd_version, payload)
            known["pid"] = handle.pid
            key = sandbox_id, handle.pid
            previous = self._commands.get(key)
            if previous is not None and previous.handle is not None:
                handle.disconnect()
                raise RequestError("commandIdentityConflict", "A new command conflicts with an existing pending process identity")
            self._commands.pop(key, None)
            self._commands[key] = _CommandEvidence(handle=handle, deadline=monotonic() + payload["timeoutSeconds"])
            return {"sandboxId": sandbox_id, "pid": handle.pid}
        if operation == "cancelCommand":
            return {"sandboxId": sandbox_id, "pid": payload["pid"], "signalSent": sandbox.commands.kill(payload["pid"], request_timeout=self.config.request_timeout_seconds)}
        key = sandbox_id, payload["pid"]
        evidence = self._commands.get(key)
        if evidence is None:
            raise RequestError("commandOutputUnavailable", "Original command output evidence is unavailable; do not reexecute the command")
        if evidence.result is not None:
            if evidence.result["stdoutByteLength"] + evidence.result["stderrByteLength"] > payload.get("maxOutputBytes", MAX_OUTPUT_BYTES):
                raise RequestError("outputLimitExceeded", "Recorded command output exceeds the requested output budget")
            self._commands.move_to_end(key)
            return dict(evidence.result)
        handle = evidence.handle
        try:
            completed = _collect_command_output(handle, sandbox_id, payload["pid"], payload.get("maxOutputBytes", MAX_OUTPUT_BYTES))
        finally:
            self._commands.pop(key, None)
        self._commands[key] = _CommandEvidence(handle=None, deadline=0, result=completed)
        return dict(completed)


def _collect_command_output(handle, sandbox_id, pid, max_output_bytes):
    stdout, stderr = bytearray(), bytearray()
    end = None
    try:
        for out, err, completion in handle:
            if end is not None:
                raise RequestError("commandStreamInvalid", "Command data followed its completion event")
            for chunk, target in ((out, stdout), (err, stderr)):
                if chunk is not None:
                    if not isinstance(chunk, bytes):
                        raise RequestError("commandStreamInvalid", "Command output must contain raw bytes")
                    if len(stdout) + len(stderr) + len(chunk) > max_output_bytes:
                        raise RequestError("outputLimitExceeded", "Output limit reached; process completion is unknown")
                    target.extend(chunk)
            if completion is not None:
                if (completion.exited is not True
                        or type(completion.exit_code) is not int or not -(2**31) <= completion.exit_code <= 2**31 - 1):
                    raise RequestError("commandOutputIncomplete", "Provider did not establish a complete process exit")
                end = completion
        if end is None:
            raise RequestError("commandOutputIncomplete", "Command stream ended without a complete exit event")
        completed = {"sandboxId": sandbox_id, "pid": pid, "exitCode": end.exit_code,
                     "stdout": stdout.decode("utf-8", errors="replace"), "stderr": stderr.decode("utf-8", errors="replace"),
                     "stdoutBytesBase64": base64.b64encode(stdout).decode("ascii"),
                     "stderrBytesBase64": base64.b64encode(stderr).decode("ascii"),
                     "stdoutByteLength": len(stdout), "stderrByteLength": len(stderr)}
    finally:
        handle.disconnect()
    return completed


def _strict_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("Duplicate JSON fields are unsupported")
        result[key] = value
    return result


def serve(adapter: Adapter, source: BinaryIO, output: BinaryIO, *, max_frame_bytes=MAX_FRAME_BYTES):
    while True:
        frame = source.readline(max_frame_bytes + 1)
        if not frame:
            return
        if len(frame) > max_frame_bytes:
            while frame and not frame.endswith(b"\n"):
                frame = source.readline(max_frame_bytes + 1)
            response = Adapter._response(None, "notDispatched", error={"code": "frameTooLarge", "message": "Adapter request frame exceeds the limit"})
        else:
            try:
                value = json.loads(frame, object_pairs_hook=_strict_object)
            except (ValueError, UnicodeError):
                response = Adapter._response(None, "notDispatched", error={"code": "invalidRequest", "message": "Invalid JSON request"})
            else:
                response = adapter.handle(value)
        encoded = json.dumps(response, ensure_ascii=True, separators=(",", ":")).encode("utf-8") + b"\n"
        if len(encoded) > MAX_FRAME_BYTES:
            encoded = json.dumps(Adapter._response(response["requestId"], "unknown", error={"code": "responseTooLarge", "message": "Provider response exceeds the adapter frame limit"})).encode() + b"\n"
        output.write(encoded)
        output.flush()
