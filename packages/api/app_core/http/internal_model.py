import json
import re
import threading

from asgiref.sync import sync_to_async
from django.http import JsonResponse
from ninja import Router

from app_core.model_adapter import (
    ModelProviderError,
    encode_model_stream_event,
    run_model,
    safe_model_error_reason,
    stream_model_async,
    validate_prepared_prompt,
)
from app_core.models import ModelConfig, AgentRunAuthorization
from app_core.model_adapter.common import PREPARED_PROMPT_FIELDS
from app_core.runtime_contract import (
    MODEL_RUN_SCHEMA,
    agent_run_binding_matches,
    _validated_authorization_digest,
    validate_agent_run_authorization_payload,
)
from app_core.workspace_access import agent_run_membership_is_current
from app_core.agent_message_prompt import conversation_provider_prompt

from .security import internal_token_auth
from .stream_response import OwnedAsyncStreamingHttpResponse


router = Router(tags=["internal"], by_alias=True)
MODEL_RUN_MAX_BODY_BYTES = 128 * 1024 * 1024


async def _model_stream_response(source):
    """Carry provider failure facts to Runtime instead of presenting an EOF."""
    terminal_delivered = False
    try:
        async for event in source:
            terminal_delivered = terminal_delivered or event.startswith(b"event: result\n")
            yield event
    except Exception as error:
        if terminal_delivered:
            raise
        reason = safe_model_error_reason(error)
        if not re.fullmatch(r"[a-z][a-z0-9_]{0,127}", reason):
            reason = "model_adapter_failed"
        yield encode_model_stream_event("error", {
            "reasonType": reason,
            "httpStatus": error.httpStatus if isinstance(error, ModelProviderError) else None,
        })
    finally:
        await source.aclose()


@router.post(
    "/model-runs",
    auth=internal_token_auth,
    response=None,
    include_in_schema=False,
)
async def model_runs(request):
    # This authenticated transport carries inline base64 images. Bound its stream
    # locally instead of relaxing Django's body limit for every public endpoint.
    raw_body = request.read(MODEL_RUN_MAX_BODY_BYTES + 1)
    if len(raw_body) > MODEL_RUN_MAX_BODY_BYTES:
        return JsonResponse({"error": "model_run_request_too_large"}, status=413)
    try:
        body = json.loads(raw_body.decode("utf-8")) if raw_body else {}
    except (UnicodeDecodeError, json.JSONDecodeError):
        return JsonResponse({"error": "invalid_json"}, status=400)
    prepared = await _validate_model_run(body)
    if isinstance(prepared, JsonResponse):
        return prepared
    model_config_ref = prepared
    agent_run_id = str(body["agentRunId"])
    if request.headers.get("Accept") == "text/event-stream":
        response = OwnedAsyncStreamingHttpResponse(
            _model_stream_response(stream_model_async(agent_run_id, model_config_ref, body)),
            content_type="text/event-stream",
        )
        response["Cache-Control"] = "no-cache"
        response["X-Accel-Buffering"] = "no"
        return response
    cancel_event = threading.Event()
    try:
        result = await sync_to_async(run_model, thread_sensitive=True)(
            agent_run_id=agent_run_id,
            model_config_ref=model_config_ref,
            request_body=body,
            cancel_event=cancel_event,
        )
    except ModelConfig.DoesNotExist:
        return JsonResponse({"error": "model_not_found"}, status=404)
    except Exception as error:
        return JsonResponse(
            {
                "error": "model_run_failed",
                "reasonType": safe_model_error_reason(error),
            },
            status=502,
        )
    finally:
        cancel_event.set()
    return JsonResponse(result)


@sync_to_async(thread_sensitive=True)
def _validate_model_run(body):
    if not isinstance(body, dict):
        return JsonResponse({"error": "invalid_json"}, status=400)
    if body.get("schema") != MODEL_RUN_SCHEMA:
        return JsonResponse({"error": "schema_mismatch"}, status=400)
    allowed_model_run_fields = {
        "schema",
        "agentRunId",
        "modelConfigRef",
        "thinkingMode",
        "authorizationRef",
        "authorizationDigest",
        "maxOutputTokens",
        "preparedPrompt",
    }
    unexpected_model_run_fields = sorted(set(body) - allowed_model_run_fields)
    if unexpected_model_run_fields:
        return JsonResponse(
            {
                "error": "model_run_fields_invalid",
                "fields": unexpected_model_run_fields,
            },
            status=400,
        )
    authorization_ref = str(body.get("authorizationRef", "")).strip()
    expected_authorization_digest = str(body.get("authorizationDigest", "")).strip()
    if not authorization_ref:
        return JsonResponse({"error": "agent_run_authorization_required"}, status=400)
    if not expected_authorization_digest:
        return JsonResponse(
            {"error": "agent_run_authorization_digest_required"},
            status=400,
        )
    authorization = AgentRunAuthorization.objects.select_related(
        "agent_run",
        "agent_run__modelConfig",
        "agent_run__session",
    ).filter(
        id=authorization_ref,
        agent_run_id=str(body.get("agentRunId", "")),
        digest=expected_authorization_digest,
    ).first()
    if authorization is None:
        return JsonResponse({"error": "agent_run_authorization_not_found"}, status=404)
    if not agent_run_membership_is_current(authorization.agent_run):
        return JsonResponse({"error": "agent_run_authorization_not_found"}, status=404)
    try:
        validate_agent_run_authorization_payload(authorization.payload)
    except ValueError:
        return JsonResponse({"error": "agent_run_authorization_invalid"}, status=409)
    if _validated_authorization_digest(authorization.payload) != authorization.digest:
        return JsonResponse(
            {"error": "agent_run_authorization_digest_mismatch"},
            status=409,
        )
    if (
        not agent_run_binding_matches(authorization.payload, authorization.agent_run)
        or str(body.get("modelConfigRef", ""))
        != authorization.payload["modelConfigRef"]
        or body.get("thinkingMode") != authorization.payload["thinkingMode"]
    ):
        return JsonResponse(
            {"error": "agent_run_authorization_binding_mismatch"},
            status=409,
        )
    max_output_tokens = body.get("maxOutputTokens")
    if (
        not isinstance(max_output_tokens, int)
        or isinstance(max_output_tokens, bool)
        or max_output_tokens <= 0
        or max_output_tokens > authorization.agent_run.modelConfig.maxOutputTokens
    ):
        return JsonResponse({"error": "model_output_limit_mismatch"}, status=409)
    prepared_prompt = body.get("preparedPrompt")
    if not isinstance(prepared_prompt, dict):
        return JsonResponse({"error": "prepared_prompt_required"}, status=400)
    if prepared_prompt.get("schema") != "prepared_prompt.v1":
        return JsonResponse(
            {"error": "prepared_prompt_schema_invalid"},
            status=400,
        )
    unexpected_prepared_prompt_fields = sorted(
        set(prepared_prompt) - PREPARED_PROMPT_FIELDS
    )
    if unexpected_prepared_prompt_fields:
        return JsonResponse(
            {
                "error": "prepared_prompt_fields_invalid",
                "fields": unexpected_prepared_prompt_fields,
            },
            status=400,
        )
    if not isinstance(prepared_prompt.get("messages"), list):
        return JsonResponse(
            {"error": "prepared_prompt_messages_invalid"},
            status=400,
        )
    if prepared_prompt.get("maxOutputTokens") != max_output_tokens:
        return JsonResponse(
            {"error": "prepared_prompt_output_limit_mismatch"},
            status=409,
        )
    try:
        validate_prepared_prompt(authorization.agent_run.modelConfig, body)
    except ModelProviderError as error:
        return JsonResponse({"error": error.reasonType}, status=400)
    # This parsed envelope is provider-local. Derive its System channel without
    # altering Core's messages, tools, authorization or any persisted Agent fact.
    body["preparedPrompt"] = conversation_provider_prompt(
        authorization.agent_run, prepared_prompt,
    )
    return authorization.payload["modelConfigRef"]
