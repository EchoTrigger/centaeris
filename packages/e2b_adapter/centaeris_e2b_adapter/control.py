"""Read-only provider attachment for a fixed trusted template helper set."""

from dataclasses import dataclass, field

CONTROL_HELPERS = ("prepare-workspace", "prepare-managed-workspace", "agent-process-state", "quiesce-agent-processes")
EXPECTED_METADATA_KEYS = ("centaerisAttemptId", "environmentId", "instanceId", "executionId", "agentRunId",
                          "sessionId", "workspaceId", "profileDigest", "authorizationDigest", "processingJobId")
HELPER_PATH = "/opt/centaeris/bin/execution_agent"
WORKSPACE_ROOT = "/mnt/data"
WORKSPACE_USER = "agent"
WORKSPACE_IDENTITY = "10001:10001"


class ControlHelperError(ValueError):
    def __init__(self, message):
        super().__init__(message)
        self.code = "controlHelperUnavailable"


@dataclass(frozen=True)
class ExistingConnection:
    # Detail contains an access token; never place it in repr, logs or output.
    detail: object = field(repr=False)
    connection: object = field(repr=False)
    envd_url: str = field(repr=False)


def read_existing_per_run(config, payload):
    from http import HTTPStatus
    from packaging.version import Version
    from e2b.api.client.api.sandboxes import get_sandboxes_sandbox_id
    from e2b.api.client.models.sandbox_detail import SandboxDetail
    from e2b.api.client.models.sandbox_lifecycle import SandboxLifecycle
    from e2b.api.client.models.sandbox_on_timeout import SandboxOnTimeout
    from e2b.api.client.models.sandbox_state import SandboxState
    from e2b.api.client_sync import get_api_client
    from e2b.connection_config import ConnectionConfig
    from e2b.envd.versions import ENVD_COMMANDS_STDIN

    # Rebuild only scoped SDK connection configuration; this does not connect,
    # resume, extend TTL, create, or construct a Sandbox execution handle.
    base = ConnectionConfig(api_key=config.api_key, api_url=config.api_url,
        sandbox_url=config.sandbox_url, domain=config.domain, retries=0,
        debug=False, request_timeout=config.request_timeout_seconds)
    response = get_sandboxes_sandbox_id.sync_detailed(payload["sandboxId"], client=get_api_client(base))
    detail = response.parsed
    if response.status_code != HTTPStatus.OK or not isinstance(detail, SandboxDetail):
        raise ControlHelperError("Fresh provider information is unavailable for fixed control work")
    lifecycle = detail.lifecycle
    if (detail.sandbox_id != payload["sandboxId"] or detail.template_id != payload["expectedTemplateId"]
            or detail.state is not SandboxState.RUNNING or not isinstance(lifecycle, SandboxLifecycle)
            or lifecycle.on_timeout is not SandboxOnTimeout.KILL or lifecycle.auto_resume is not False):
        raise ControlHelperError("Provider identity, running state or required provider lifecycle policy does not match")
    if not isinstance(detail.metadata, dict) or detail.metadata != payload["expectedMetadata"]:
        raise ControlHelperError("Provider identity metadata does not match the frozen creation record")
    token = detail.envd_access_token
    if not isinstance(token, str) or not token or len(token) > 8192 or any(not 33 <= ord(char) <= 126 for char in token):
        raise ControlHelperError("Secure provider control attachment is unavailable")
    if not isinstance(detail.envd_version, str) or Version(detail.envd_version) < ENVD_COMMANDS_STDIN:
        raise ControlHelperError("Provider does not support the fixed no-stdin process contract")
    connection = ConnectionConfig(extra_sandbox_headers={"E2b-Sandbox-Id": payload["sandboxId"],
        "E2b-Sandbox-Port": str(ConnectionConfig.envd_port), "X-Access-Token": token}, **base.get_api_params())
    return ExistingConnection(detail, connection, base.get_sandbox_url(payload["sandboxId"], detail.domain))


def start_control_helper(config, payload, start_process):
    from e2b.envd.process import process_pb

    context = read_existing_per_run(config, payload)
    envs = {"CENTAERIS_WORKSPACE_ROOT": WORKSPACE_ROOT, "CENTAERIS_WORKSPACE_USER": WORKSPACE_IDENTITY}
    if payload["helper"] == "prepare-managed-workspace":
        envs.update({"CENTAERIS_MANAGED_ENVIRONMENT_ID": context.detail.metadata["environmentId"],
                     "CENTAERIS_MANAGED_SANDBOX_ID": context.detail.sandbox_id})
    process = process_pb.ProcessConfig(cmd=HELPER_PATH, args=[payload["helper"]], cwd="/", envs=envs)
    user = WORKSPACE_USER if payload["helper"] == "quiesce-agent-processes" else "root"
    return start_process(context.envd_url, context.connection,
                         context.detail.envd_version, user, process, payload["timeoutSeconds"])
