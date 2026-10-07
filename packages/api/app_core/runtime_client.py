import json
import time
import urllib.error
import urllib.request

from django.conf import settings

from .transcript_contract import valid_transcript_block_structure, valid_transcript_structure

from .runtime_contract import (
    AGENT_RUN_START_SCHEMA,
    agent_run_binding_matches,
    authorization_digest,
)
from .runtime_job_client import schedule_runtime_job
from .workspace_access import agent_run_membership_is_current


AGENT_RUN_LIFECYCLE_JOB_KIND = "agent_run.lifecycle"
AGENT_RUN_LIFECYCLE_SCHEDULE_SCHEMA = "runtime.job.schedule.v1"
WORKSPACE_SKILL_CATALOG_SCHEMA = "workspace.skill.catalog.v1"
WORKSPACE_SKILL_CATALOG_RESULT_SCHEMA = "workspace.skill.catalog.result.v1"
WORKSPACE_SKILL_DETAIL_SCHEMA = "workspace.skill.detail.v1"
WORKSPACE_SKILL_DETAIL_RESULT_SCHEMA = "workspace.skill.detail.result.v1"
WORKSPACE_MCP_CATALOG_SCHEMA = "workspace.mcp.catalog.v1"
WORKSPACE_MCP_CATALOG_RESULT_SCHEMA = "workspace.mcp.catalog.result.v1"
WORKSPACE_HOOK_CATALOG_SCHEMA = "workspace.hook.catalog.v1"
WORKSPACE_HOOK_CATALOG_RESULT_SCHEMA = "workspace.hook.catalog.result.v1"
WORKSPACE_MODEL_CATALOG_RESULT_SCHEMA = "workspace.model_catalog.result.v1"
WORKSPACE_PLUGIN_INSPECT_SCHEMA = "workspace.plugin.inspect.v1"
WORKSPACE_PLUGIN_INSPECT_RESULT_SCHEMA = "workspace.plugin.inspect.result.v1"
RUNTIME_EXECUTION_PROFILE_SCHEMA = "runtime.execution_profile.v1"


def request_execution_profile() -> dict:
    request = urllib.request.Request(
        f"{settings.RUNTIME_URL.rstrip('/')}/internal/execution-profile",
        headers={"X-Internal-Token": settings.INTERNAL_API_TOKEN},
        method="GET",
    )
    try:
        with urllib.request.urlopen(
            request, timeout=settings.RUNTIME_START_TIMEOUT_SECONDS
        ) as response:
            result = json.loads(response.read())
    except (urllib.error.HTTPError, urllib.error.URLError, json.JSONDecodeError) as error:
        if isinstance(error, urllib.error.HTTPError):
            error.close()
        raise RuntimeError("runtime_execution_profile_request_failed") from error
    if (
        not isinstance(result, dict)
        or set(result) != {"schema", "imageCapability", "imageDigest"}
        or result["schema"] != RUNTIME_EXECUTION_PROFILE_SCHEMA
        or result["imageCapability"] != "workspace_general_v1"
        or not isinstance(result["imageDigest"], str)
        or len(result["imageDigest"]) != 71
        or not result["imageDigest"].startswith("sha256:")
        or any(character not in "0123456789abcdef" for character in result["imageDigest"][7:])
    ):
        raise RuntimeError("runtime_execution_profile_response_invalid")
    return result


def request_model_catalog() -> dict:
    request = urllib.request.Request(
        f"{settings.RUNTIME_URL.rstrip('/')}/internal/model-catalog",
        headers={"X-Internal-Token": settings.INTERNAL_API_TOKEN},
        method="GET",
    )
    try:
        with urllib.request.urlopen(
            request, timeout=settings.RUNTIME_START_TIMEOUT_SECONDS
        ) as response:
            result = json.loads(response.read())
    except (urllib.error.HTTPError, urllib.error.URLError, json.JSONDecodeError) as error:
        raise RuntimeError("workspace_model_catalog_request_failed") from error
    if (
        not isinstance(result, dict)
        or set(result) != {"schema", "catalog"}
        or result["schema"] != WORKSPACE_MODEL_CATALOG_RESULT_SCHEMA
    ):
        raise RuntimeError("workspace_model_catalog_response_invalid")
    catalog = result["catalog"]
    if (
        not isinstance(catalog, dict)
        or set(catalog) != {"schema", "providers"}
        or catalog["schema"] != "centaeris.model_catalog.v1"
        or not isinstance(catalog["providers"], list)
    ):
        raise RuntimeError("workspace_model_catalog_response_invalid")
    return catalog


def agent_run_lifecycle_job_id(agent_run_id: str) -> str:
    if (
        not isinstance(agent_run_id, str)
        or not 1 <= len(agent_run_id) <= 128
        or any(
            not (character.isascii() and (character.isalnum() or character in "_-."))
            for character in agent_run_id
        )
    ):
        raise ValueError("agent_run_lifecycle_agent_run_id_invalid")
    return f"agent_run.lifecycle:{agent_run_id}"


def _validate_agent_run_binding(agent_run):
    if not agent_run_membership_is_current(agent_run):
        raise RuntimeError("AgentRun WorkspaceMembership is no longer current")
    authorization = agent_run.authorization
    if authorization_digest(authorization.payload) != authorization.digest:
        raise RuntimeError("AgentRun authorization digest mismatch")
    if (
        not agent_run_binding_matches(authorization.payload, agent_run)
        or authorization.payload["thinkingMode"] != (agent_run.thinkingMode or None)
    ):
        raise RuntimeError("AgentRun authorization binding mismatch")
    return authorization


def build_agent_run_start(agent_run) -> dict:
    from .models import AgentCoordinationSession, AgentInputQueue
    authorization = _validate_agent_run_binding(agent_run)
    start = {
        "schema": AGENT_RUN_START_SCHEMA,
        "agentRunId": agent_run.id,
        "turnId": agent_run.turn_id,
        "prompt": agent_run.prompt,
        "initialInput": {"type": "userMessage"},
        "agentInstructions": agent_run.agent_instructions,
        "modelContextTokens": agent_run.modelConfig.contextTokens,
        "modelMaxOutputTokens": agent_run.modelConfig.maxOutputTokens,
        "authorizationDigest": authorization.digest,
        "authorizationSignature": authorization.signature,
        "authorization": authorization.payload,
        "tailAction": (
            {"type": "append"}
            if agent_run.tailPolicy == "append"
            else {
                "type": "rewriteLastUser",
                "targetMessageId": agent_run.rewriteTargetMessageId,
                "expectedTailMessageId": agent_run.rewriteExpectedTailMessageId,
            }
        ),
    }
    if AgentCoordinationSession.objects.filter(agent_id=agent_run.session.agent_id,
                                              session_id=agent_run.session_id).exists():
        start["coordinationSessionId"] = agent_run.session_id
        if (not agent_run.session.agent.definition_id and not agent_run.definition_version_id
                and not agent_run.acting_app_id and not agent_run.app_delegation_id):
            start["nativeCoordinationSessionId"] = agent_run.session_id
    queue = AgentInputQueue.objects.select_related("initial_host__notice__work", "initial_host__operation").filter(agent_run=agent_run).first()
    attempt = queue.initial_host if queue is not None else None
    if attempt is not None:
        from .agent_work_consumption import bound_initial_input
        start["initialInput"] = bound_initial_input(attempt, agent_run)
    from .agent_input_delivery import initial_agent_input
    initial = initial_agent_input(agent_run)
    if initial is not None:
        if attempt is not None:
            raise ValueError("agent_run_initial_input_owner_conflict")
        start["initialInput"] = initial
    return start


def request_host_event_input(session_id, input_id, source, content):
    """Core owns native input identity; the API persists its opaque result."""
    request = urllib.request.Request(
        f"{settings.RUNTIME_URL.rstrip('/')}/internal/host-event-input",
        data=json.dumps({"schema": "runtime.host_event_input.create.v1", "sessionId": session_id,
            "inputId": input_id, "source": source, "content": content}, separators=(",", ":")).encode(),
        headers={"Content-Type": "application/json", "X-Internal-Token": settings.INTERNAL_API_TOKEN}, method="POST")
    try:
        with urllib.request.urlopen(request, timeout=settings.RUNTIME_START_TIMEOUT_SECONDS) as response:
            result = json.loads(response.read())
    except (urllib.error.HTTPError, urllib.error.URLError, json.JSONDecodeError) as error:
        if isinstance(error, urllib.error.HTTPError):
            error.close()
        raise RuntimeError("host_event_input_request_failed") from error
    if (not isinstance(result, dict) or set(result) != {"schema", "input"}
            or result["schema"] != "runtime.host_event_input.created.v1"
            or not isinstance(result["input"], dict)
            or set(result["input"]) != {"inputId", "messageId", "source", "content"}
            or result["input"]["inputId"] != input_id or result["input"]["source"] != source
            or result["input"]["content"] != content
            or not isinstance(result["input"]["messageId"], str) or not result["input"]["messageId"]):
        raise RuntimeError("host_event_input_response_invalid")
    return result["input"]


def schedule_agent_run_lifecycle(agent_run) -> str:
    authorization = _validate_agent_run_binding(agent_run)
    job_id = agent_run_lifecycle_job_id(agent_run.id)
    body = {
        "schema": AGENT_RUN_LIFECYCLE_SCHEDULE_SCHEMA,
        "jobId": job_id,
        "jobKind": AGENT_RUN_LIFECYCLE_JOB_KIND,
        "runAtMs": time.time_ns() // 1_000_000,
        "maxRetries": 10,
        "idempotencyKey": f"agent_run.lifecycle:{agent_run.id}:{authorization.digest}",
        "sessionId": agent_run.session_id,
        "payloadRef": f"record:agent_run:{agent_run.id}",
        "workspaceId": agent_run.workspace_id,
    }
    payload = schedule_runtime_job(body)
    job = payload.get("job") if isinstance(payload, dict) else None
    if (
        payload.get("disposition") not in {"inserted", "existing"}
        or not isinstance(job, dict)
        or job.get("jobId") != job_id
        or job.get("jobKind") != AGENT_RUN_LIFECYCLE_JOB_KIND
        or job.get("idempotencyKey") != body["idempotencyKey"]
        or job.get("sessionId") != agent_run.session_id
        or job.get("payloadRef") != body["payloadRef"]
        or job.get("status") not in {"queued", "leased", "running"}
    ):
        raise RuntimeError("agent_run_lifecycle_schedule_response_invalid")
    return payload["disposition"]


def request_agent_run_cancellation(agent_run) -> dict:
    body = {
        "schema": "runtime.agent_run.cancel.v1",
        "agentRunStart": build_agent_run_start(agent_run),
    }
    request = urllib.request.Request(
        f"{settings.RUNTIME_URL.rstrip('/')}/agent-runs/cancel",
        data=json.dumps(body, separators=(",", ":")).encode(),
        headers={
            "Content-Type": "application/json",
            "X-Internal-Token": settings.INTERNAL_API_TOKEN,
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(
            request, timeout=settings.RUNTIME_CONTROL_TIMEOUT_SECONDS
        ) as response:
            result = json.loads(response.read())
    except (
        urllib.error.HTTPError,
        urllib.error.URLError,
        TimeoutError,
        json.JSONDecodeError,
    ) as error:
        if isinstance(error, urllib.error.HTTPError):
            error.close()
        raise RuntimeError("agent_run_cancel_request_failed") from error
    if (
        not isinstance(result, dict)
        or set(result) != {"schema", "agentRunId", "disposition", "terminalState"}
        or result["schema"] != "runtime.agent_run.cancel.result.v1"
        or result["agentRunId"] != agent_run.id
        or result["disposition"] not in {"requested", "terminal"}
        or (
            result["disposition"] == "requested" and result["terminalState"] is not None
        )
        or (
            result["disposition"] == "terminal"
            and result["terminalState"] not in {"completed", "failed", "cancelled"}
        )
    ):
        raise RuntimeError("agent_run_cancel_response_invalid")
    return result


def request_agent_run_supplement(agent_run, supplement_id: str, message: str) -> dict:
    body = {
        "schema": "runtime.agent_run.supplement.v1",
        "supplementId": supplement_id,
        "jobId": agent_run_lifecycle_job_id(agent_run.id),
        "message": message,
        "agentRunStart": build_agent_run_start(agent_run),
    }
    request = urllib.request.Request(
        f"{settings.RUNTIME_URL.rstrip('/')}/internal/agent-runs/supplement",
        data=json.dumps(body, separators=(",", ":")).encode(),
        headers={
            "Content-Type": "application/json",
            "X-Internal-Token": settings.INTERNAL_API_TOKEN,
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(
            request, timeout=settings.RUNTIME_START_TIMEOUT_SECONDS
        ) as response:
            result = json.loads(response.read())
    except urllib.error.HTTPError as error:
        try:
            payload = json.loads(error.read())
        except json.JSONDecodeError:
            payload = None
        finally:
            error.close()
        if (
            error.code == 409
            and isinstance(payload, dict)
            and set(payload) == {"error"}
            and isinstance(payload["error"], str)
            and payload["error"]
        ):
            raise ValueError(payload["error"]) from error
        raise RuntimeError("agent_run_supplement_request_failed") from error
    except (urllib.error.URLError, json.JSONDecodeError) as error:
        raise RuntimeError("agent_run_supplement_request_failed") from error
    if (
        not isinstance(result, dict)
        or set(result)
        != {
            "schema",
            "accepted",
            "disposition",
            "agentRunId",
            "sessionId",
            "supplementId",
            "queuedCount",
            "queueRevision",
        }
        or result["schema"] != "runtime.agent_run.supplement.result.v1"
        or result["accepted"] is not True
        or not isinstance(result["disposition"], str)
        or result["disposition"] not in {"accepted", "duplicate"}
        or result["agentRunId"] != agent_run.id
        or result["sessionId"] != agent_run.session_id
        or result["supplementId"] != supplement_id
        or type(result["queuedCount"]) is not int
        or not 0 <= result["queuedCount"] <= 8
        or type(result["queueRevision"]) is not int
        or result["queueRevision"] < 1
    ):
        raise RuntimeError("agent_run_supplement_response_invalid")
    return result


def request_workspace_skill_catalog(plugin_activation: dict) -> dict:
    return _request_workspace_projection(
        "/skills/catalog",
        {
            "schema": WORKSPACE_SKILL_CATALOG_SCHEMA,
            "pluginActivation": plugin_activation,
        },
        {"schema", "skills"},
        WORKSPACE_SKILL_CATALOG_RESULT_SCHEMA,
    )


def request_workspace_skill_detail(plugin_activation: dict, skill_id: str) -> dict:
    if not isinstance(skill_id, str) or not skill_id.strip():
        raise ValueError("workspace_skill_id_invalid")
    return _request_workspace_projection(
        "/skills/detail",
        {
            "schema": WORKSPACE_SKILL_DETAIL_SCHEMA,
            "pluginActivation": plugin_activation,
            "skillId": skill_id,
        },
        {"schema", "skill", "content"},
        WORKSPACE_SKILL_DETAIL_RESULT_SCHEMA,
        not_found_error="skill_not_found",
    )


def request_workspace_mcp_catalog(plugin_activation: dict) -> dict:
    return _request_workspace_projection(
        "/mcp/catalog",
        {
            "schema": WORKSPACE_MCP_CATALOG_SCHEMA,
            "pluginActivation": plugin_activation,
        },
        {"schema", "plugins"},
        WORKSPACE_MCP_CATALOG_RESULT_SCHEMA,
    )


def request_workspace_hook_catalog(plugin_activation: dict) -> dict:
    return _request_workspace_projection(
        "/hooks/catalog",
        {
            "schema": WORKSPACE_HOOK_CATALOG_SCHEMA,
            "pluginActivation": plugin_activation,
        },
        {"schema", "plugins"},
        WORKSPACE_HOOK_CATALOG_RESULT_SCHEMA,
    )


def request_plugin_inspection(package_path: str) -> dict:
    if (
        not isinstance(package_path, str)
        or not package_path
        or package_path.startswith("/")
        or "\\" in package_path
        or ":" in package_path
        or any(part in {"", ".", ".."} for part in package_path.split("/"))
    ):
        raise ValueError("plugin_inspection_path_invalid")
    body = {
        "schema": WORKSPACE_PLUGIN_INSPECT_SCHEMA,
        "packagePath": package_path,
    }
    request = urllib.request.Request(
        f"{settings.RUNTIME_URL.rstrip('/')}/internal/plugins/inspect",
        data=json.dumps(body, separators=(",", ":")).encode(),
        headers={
            "Content-Type": "application/json",
            "X-Internal-Token": settings.INTERNAL_API_TOKEN,
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(
            request, timeout=settings.RUNTIME_START_TIMEOUT_SECONDS
        ) as response:
            result = json.loads(response.read())
    except urllib.error.HTTPError as error:
        try:
            payload = json.loads(error.read())
        except json.JSONDecodeError:
            payload = None
        finally:
            error.close()
        if error.code == 400 and payload == {"error": "plugin_package_invalid"}:
            raise ValueError("plugin_package_invalid") from error
        raise RuntimeError("plugin_inspection_request_failed") from error
    except (urllib.error.URLError, json.JSONDecodeError) as error:
        raise RuntimeError("plugin_inspection_request_failed") from error
    if (
        not isinstance(result, dict)
        or set(result) != {"schema", "package"}
        or result["schema"] != WORKSPACE_PLUGIN_INSPECT_RESULT_SCHEMA
        or not isinstance(result["package"], dict)
    ):
        raise RuntimeError("plugin_inspection_response_invalid")
    return result["package"]


def _request_workspace_projection(
    path: str,
    body: dict,
    expected_fields: set[str],
    expected_schema: str,
    *,
    not_found_error: str | None = None,
) -> dict:
    request = urllib.request.Request(
        f"{settings.RUNTIME_URL.rstrip('/')}{path}",
        data=json.dumps(body, separators=(",", ":")).encode(),
        headers={
            "Content-Type": "application/json",
            "X-Internal-Token": settings.INTERNAL_API_TOKEN,
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(
            request, timeout=settings.RUNTIME_START_TIMEOUT_SECONDS
        ) as response:
            result = json.loads(response.read())
    except urllib.error.HTTPError as error:
        try:
            payload = json.loads(error.read())
        except json.JSONDecodeError:
            payload = None
        finally:
            error.close()
        if error.code == 404 and payload == {"error": not_found_error}:
            raise LookupError(not_found_error) from error
        raise RuntimeError("workspace_runtime_projection_request_failed") from error
    except (urllib.error.URLError, json.JSONDecodeError) as error:
        raise RuntimeError("workspace_runtime_projection_request_failed") from error
    if (
        not isinstance(result, dict)
        or set(result) != expected_fields
        or result.get("schema") != expected_schema
    ):
        raise RuntimeError("workspace_runtime_projection_response_invalid")
    return result


class TranscriptRuntimeError(RuntimeError):
    def __init__(self, payload: dict):
        super().__init__(payload.get("error", "transcript_runtime_request_failed"))
        self.payload = payload


TRANSCRIPT_PAGE_MAX_BLOCKS = 128
TRANSCRIPT_PAGE_MAX_INLINE_BYTES = 64 * 1024
TRANSCRIPT_PAGE_MAX_SERIALIZED_BYTES = 4 * TRANSCRIPT_PAGE_MAX_INLINE_BYTES
TRANSCRIPT_PATCH_PAGE_MAX_PATCHES = TRANSCRIPT_PAGE_MAX_BLOCKS
TRANSCRIPT_PATCH_MAX_CHANGES = TRANSCRIPT_PAGE_MAX_BLOCKS
TRANSCRIPT_PATCH_MAX_SERIALIZED_BYTES = TRANSCRIPT_PAGE_MAX_SERIALIZED_BYTES
TRANSCRIPT_PATCH_PAGE_MAX_SERIALIZED_BYTES = 2 * TRANSCRIPT_PATCH_MAX_SERIALIZED_BYTES
WORKSPACE_TRANSCRIPT_STREAM_ID = "workspace-transcript.v1"


def request_transcript_content(body: dict) -> dict:
    result = _request_transcript_read("/internal/transcript/content", body)
    if (not valid_transcript_structure("content", result)
        or result.get("schema") != "transcript.content.range.v1"
        or any(result.get(key) != body.get(key) for key in
               ("sessionId", "projectionVersion", "projectionGeneration", "refId", "revision", "byteLength"))
        or result.get("startOffset") != body.get("offset")
        or not _canonical_decimal_u64(result.get("endOffset"))
    ):
        raise RuntimeError("transcript_content_response_invalid")
    start, end, length = int(result["startOffset"]), int(result["endOffset"]), int(result["byteLength"])
    if (not start <= end <= length
        or end - start != len(result["content"].encode("utf-8"))
        or end - start > body["maxBytes"]
        or (end == start and end < length)
        or result["hasMore"] != (end < length)):
        raise RuntimeError("transcript_content_response_invalid")
    return result


def request_transcript_page(body: dict) -> dict:
    result = _request_transcript_read("/internal/transcript/page", body)
    if (
        not valid_transcript_structure("page", result)
        or result.get("schema") != "transcript.page.v1"
        or result.get("sessionId") != body.get("sessionId")
        or result.get("projectionVersion") != body.get("projectionVersion")
        or (
            body.get("projectionGeneration") is not None
            and result.get("projectionGeneration") != body.get("projectionGeneration")
        )
        or result.get("sourceHighWater") != body.get("sourceHighWater")
        or not _valid_transcript_page_content(result)
    ):
        raise RuntimeError("transcript_page_response_invalid")
    return result


def request_transcript_patches(body: dict) -> dict:
    result = _request_transcript_read("/internal/transcript/patches", body)
    if (
        not valid_transcript_structure("patches", result)
        or result.get("schema") != "transcript.patch.page.v1"
        or result.get("sessionId") != body.get("sessionId")
        or result.get("projectionVersion") != body.get("projectionVersion")
        or result.get("projectionGeneration") != body.get("projectionGeneration")
        or result.get("throughSourceHighWater") != body.get("throughSourceHighWater")
        or not _valid_transcript_patch_content(result, body)
    ):
        raise RuntimeError("transcript_patch_response_invalid")
    return result


def _request_transcript_read(path: str, body: dict) -> dict:
    request = urllib.request.Request(
        f"{settings.RUNTIME_URL.rstrip('/')}{path}",
        data=json.dumps(body, separators=(",", ":")).encode(),
        headers={
            "Content-Type": "application/json",
            "X-Internal-Token": settings.INTERNAL_API_TOKEN,
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(
            request, timeout=settings.RUNTIME_START_TIMEOUT_SECONDS
        ) as response:
            result = json.loads(response.read())
    except urllib.error.HTTPError as error:
        try:
            payload = json.loads(error.read())
        except json.JSONDecodeError:
            payload = None
        finally:
            error.close()
        if error.code == 409 and isinstance(payload, dict):
            conflict = _validate_transcript_conflict(payload, body)
            if conflict is not None:
                raise TranscriptRuntimeError(conflict) from error
        raise RuntimeError("transcript_runtime_request_failed") from error
    except (urllib.error.URLError, json.JSONDecodeError) as error:
        raise RuntimeError("transcript_runtime_request_failed") from error
    if not isinstance(result, dict):
        raise RuntimeError("transcript_runtime_response_invalid")
    return result


def _validate_transcript_conflict(payload: dict, request_body: dict) -> dict | None:
    if valid_transcript_structure("error", payload) and payload["error"] in {
        "transcript_content_unavailable", "transcript_view_invalidated"
    }:
        return payload
    if (not valid_transcript_structure("projectionNotReady", payload)
        or payload["error"] != "transcript_projection_not_ready"):
        return None
    generation = payload.get("projectionGeneration")
    requested_generation = request_body.get("projectionGeneration")
    source_high_water = payload.get("sourceHighWater")
    projected_high_water = payload.get("projectedHighWater")
    if (
        not isinstance(generation, str)
        or not generation
        or (
            requested_generation is not None
            and generation != requested_generation
        )
        or source_high_water
        not in {
            request_body.get("sourceHighWater"),
            request_body.get("throughSourceHighWater"),
        }
        or not _canonical_decimal_u64(source_high_water)
        or not _canonical_decimal_u64(projected_high_water)
        or int(projected_high_water) >= int(source_high_water)
    ):
        return None
    return payload


def _canonical_decimal_u64(value: object) -> bool:
    return (
        isinstance(value, str)
        and value.isascii()
        and value.isdigit()
        and (value == "0" or not value.startswith("0"))
        and len(value) <= 20
        and int(value) <= 18_446_744_073_709_551_615
    )


def _valid_transcript_page_content(page: dict) -> bool:
    if (
        len(json.dumps(page, separators=(",", ":")).encode())
        > TRANSCRIPT_PAGE_MAX_SERIALIZED_BYTES
    ):
        return False
    high_water = page.get("sourceHighWater")
    if not _canonical_decimal_u64(high_water):
        return False
    blocks = page.get("blocks")
    if len(blocks) > TRANSCRIPT_PAGE_MAX_BLOCKS or not all(
        _valid_transcript_block(block) for block in blocks
    ):
        return False
    orders = [_transcript_block_order(block) for block in blocks]
    if any(order is None or order[0] > int(high_water) for order in orders):
        return False
    if any(left >= right for left, right in zip(orders, orders[1:])):
        return False
    if len({block["blockId"] for block in blocks}) != len(blocks):
        return False
    if (
        sum(_transcript_inline_bytes(block) for block in blocks)
        > TRANSCRIPT_PAGE_MAX_INLINE_BYTES
    ):
        return False
    if page.get("hasOlder") != (page.get("olderCursor") is not None):
        return False
    cursors = page.get("resumeCursors")
    if high_water == "0":
        return cursors == []
    return (
        len(cursors) == 1
        and cursors[0].get("streamId") == WORKSPACE_TRANSCRIPT_STREAM_ID
        and _canonical_decimal_u64(cursors[0].get("cursor"))
        and int(cursors[0]["cursor"]) <= int(high_water)
    )


def _valid_transcript_patch_content(page: dict, request: dict) -> bool:
    if (
        len(json.dumps(page, separators=(",", ":")).encode())
        > TRANSCRIPT_PATCH_PAGE_MAX_SERIALIZED_BYTES
    ):
        return False
    through = page.get("throughSourceHighWater")
    next_water = page.get("nextSourceHighWater")
    after = request.get("afterSourceHighWater")
    if (
        not _canonical_decimal_u64(through)
        or not _canonical_decimal_u64(next_water)
        or not _canonical_decimal_u64(after)
        or int(next_water) < int(after)
    ):
        return False
    patches = page.get("patches")
    if len(patches) > TRANSCRIPT_PATCH_PAGE_MAX_PATCHES:
        return False
    previous = int(after)
    for patch in patches:
        if not _valid_transcript_patch(patch, page):
            return False
        water = int(patch["sourceHighWater"])
        if water <= previous:
            return False
        previous = water
    if patches and next_water != patches[-1]["sourceHighWater"]:
        return False
    if not patches and (next_water != through or after != through):
        return False
    return (int(next_water) < int(through)) == page.get("hasMore")


def _valid_transcript_patch(patch: object, page: dict) -> bool:
    if (
        patch.get("schema") != "transcript.patch.v1"
        or patch.get("sessionId") != page.get("sessionId")
        or patch.get("projectionVersion") != page.get("projectionVersion")
        or patch.get("projectionGeneration") != page.get("projectionGeneration")
        or patch.get("streamId") != WORKSPACE_TRANSCRIPT_STREAM_ID
        or patch.get("appliedCursor") != patch.get("sourceHighWater")
        or not _canonical_decimal_u64(patch.get("sourceHighWater"))
        or int(patch["sourceHighWater"]) > int(page["throughSourceHighWater"])
        or len(patch["upserts"]) + len(patch["removals"])
        > TRANSCRIPT_PATCH_MAX_CHANGES
        or not all(_valid_transcript_block(block) for block in patch["upserts"])
        or not all(
            bool(removal["blockId"].strip())
            and _canonical_decimal_u64(removal.get("blockRevision"))
            for removal in patch["removals"]
        )
    ):
        return False
    return (
        len(json.dumps(patch, separators=(",", ":")).encode())
        <= TRANSCRIPT_PATCH_MAX_SERIALIZED_BYTES
    )


def _valid_transcript_block(block: object) -> bool:
    if not valid_transcript_block_structure(block):
        return False
    if (
        not isinstance(block.get("blockId"), str)
        or not block["blockId"].strip()
        or not _canonical_decimal_u64(block.get("blockRevision"))
        or _transcript_block_order(block) is None
    ):
        return False
    body = block.get("body")
    kind = body["kind"]
    if kind == "tool":
        summary = body["summary"]
        summary_ref = body["summaryRef"]
        return (
            all(
                isinstance(body.get(field), str) and body[field].strip()
                for field in ("callId", "toolName")
            )
            and (
                (
                    isinstance(summary, str)
                    and len(summary.encode()) <= TRANSCRIPT_PAGE_MAX_INLINE_BYTES
                    and summary_ref is None
                )
                or (summary is None and _valid_transcript_content_ref(summary_ref))
            )
            and (body["outputRef"] is None or _valid_transcript_content_ref(body["outputRef"]))
        )
    if kind == "reasoning" and (
        not isinstance(body.get("requestId"), str) or not body["requestId"].strip()
    ):
        return False
    if kind == "notice" and (
        not isinstance(body.get("noticeType"), str) or not body["noticeType"].strip()
    ):
        return False
    return _valid_transcript_content(body.get("content"))


def _transcript_block_order(block: dict) -> tuple[int, int] | None:
    order = block.get("orderKey")
    if (
        not isinstance(order, dict)
        or set(order) != {"sourceSequence", "ordinal"}
        or not _canonical_decimal_u64(order.get("sourceSequence"))
        or type(order.get("ordinal")) is not int
        or not 0 <= order["ordinal"] <= 4_294_967_295
    ):
        return None
    return int(order["sourceSequence"]), order["ordinal"]


def _valid_transcript_content(content: object) -> bool:
    inline = content.get("inlineContent")
    reference = content.get("sourceRef")
    return (
        isinstance(inline, str)
        and reference is None
        and len(inline.encode()) <= TRANSCRIPT_PAGE_MAX_INLINE_BYTES
    ) or (inline is None and _valid_transcript_content_ref(reference))


def _valid_transcript_content_ref(reference: object) -> bool:
    return (
        reference is not None
        and bool(reference["refId"].strip())
        and _canonical_decimal_u64(reference.get("revision"))
        and _canonical_decimal_u64(reference.get("byteLength"))
    )


def _transcript_inline_bytes(block: dict) -> int:
    body = block["body"]
    if body["kind"] == "tool":
        return len((body["summary"] or "").encode())
    return len((body["content"].get("inlineContent") or "").encode())
