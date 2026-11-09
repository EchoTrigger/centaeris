from base64 import urlsafe_b64encode
import hashlib
import hmac
import json
from pathlib import Path
import re
import secrets
import threading

from asgiref.sync import sync_to_async
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from django.conf import settings
from django.http import HttpResponse, JsonResponse
from django.db import connection
from ninja import Router

from app_core.api_memory import bind_agent_run
from app_core.credentials import CredentialDecryptionError, decrypt_credential_secret
from app_core.model_adapter import (
    ModelProviderError,
    encode_model_stream_event,
    run_model,
    safe_model_error_reason,
    stream_model_async,
    validate_prepared_prompt,
)
from app_core.models import (
    AgentRunAuthorization,
    ModelConfig,
    ModelEndpointValidationError,
    ProviderCredential,
    validate_model_endpoint,
)
from app_core.model_adapter.common import PREPARED_PROMPT_FIELDS
from app_core.model_adapter.common import model_database_call
from app_core.runtime_contract import (
    MODEL_RUN_SCHEMA,
    agent_run_binding_matches,
    _validated_authorization_digest,
    _verify_authorization_digest_signature,
    require_sha256,
    validate_agent_run_authorization_payload,
)
from app_core.workspace_access import agent_run_membership_is_current

from .security import internal_token_auth
from .stream_response import OwnedAsyncStreamingHttpResponse


router = Router(tags=["internal"], by_alias=True)
MODEL_RUN_MAX_BODY_BYTES = 128 * 1024 * 1024
NATIVE_MODEL_GRANT_FIELDS = (
    "agentRunId", "authorizationRef", "authorizationDigest", "leaseOwner", "providerId",
)
NATIVE_MODEL_AUTHORITY_SQL = re.sub(
    r"\$[1-5]", "%s",
    Path(__file__).resolve().parents[1].joinpath("native_model_authority.sql").read_text(encoding="utf-8"),
)


def _native_model_grant_request(raw_body):
    if len(raw_body) > 4096:
        raise ValueError("native_model_grant_request_too_large")
    body = json.loads(raw_body)
    if (not isinstance(body, dict)
            or set(body) != {*NATIVE_MODEL_GRANT_FIELDS, "schema", "proof"}
            or body["schema"] != "runtime.native_model_grant.request.v1"):
        raise ValueError("native_model_grant_request_invalid")
    for field in ("agentRunId", "authorizationRef", "providerId"):
        value = body[field]
        if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.:-]{0,63}", value):
            raise ValueError("native_model_grant_identity_invalid")
    require_sha256("authorizationDigest", body["authorizationDigest"])
    owner = body["leaseOwner"]
    if (not isinstance(owner, str) or not 16 <= len(owner.encode("utf-8")) <= 160
            or any(ord(char) < 32 or 127 <= ord(char) < 160 for char in owner)):
        raise ValueError("native_model_grant_lease_owner_invalid")
    proof = body["proof"]
    if not isinstance(proof, str) or not re.fullmatch(r"hmac-sha256:[0-9a-f]{64}", proof):
        raise ValueError("native_model_grant_proof_invalid")
    return tuple(body[field] for field in NATIVE_MODEL_GRANT_FIELDS), proof


@router.post(
    "/model-runs/native-grant",
    auth=internal_token_auth,
    response=None,
    include_in_schema=False,
)
async def native_model_grant(request):
    try:
        values, proof = _native_model_grant_request(request.read(4097))
    except ValueError:
        response = JsonResponse({"error": "native_model_grant_invalid"}, status=400)
    else:
        message = "workspace:native-model-grant:v1\0" + "\0".join(values)
        expected = "hmac-sha256:" + hmac.digest(
            settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY.encode("utf-8"), message.encode("utf-8"), "sha256",
        ).hex()
        if hmac.compare_digest(proof, expected):
            response = await _resolve_native_model_grant(values, proof)
        else:
            response = JsonResponse({"error": "native_model_grant_proof_invalid"}, status=403)
    response.headers["Cache-Control"] = "no-store"
    return response


@model_database_call
def _resolve_native_model_grant(values, proof):
    run_id, authorization_ref, digest, lease_owner, provider_id = values
    denied = {"error": "native_model_authority_denied"}
    authorization = AgentRunAuthorization.objects.select_related(
        "agent_run__modelConfig__provider", "agent_run__session", "agent_run__environment",
    ).filter(id=authorization_ref, agent_run_id=run_id, digest=digest).first()
    if authorization is None or not agent_run_membership_is_current(authorization.agent_run):
        return JsonResponse(denied, status=409)
    run = authorization.agent_run
    payload = authorization.payload
    try:
        validate_agent_run_authorization_payload(payload)
        computed_digest = _validated_authorization_digest(payload)
        _verify_authorization_digest_signature(
            computed_digest, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY,
            authorization.signature, schema=payload["schema"],
        )
    except ValueError:
        return JsonResponse(denied, status=409)
    if (computed_digest != digest or payload["id"] != authorization_ref
            or not agent_run_binding_matches(payload, run)
            or payload["thinkingMode"] != (run.thinkingMode or None)):
        return JsonResponse(denied, status=409)
    model = run.modelConfig
    provider = model.provider
    if provider is not None and (
        provider.workspace_id != run.workspace_id or not provider.enabled or provider.archivedAt is not None
    ):
        return JsonResponse(denied, status=409)
    selected = (
        provider is not None and provider.id == provider_id and run.session.purpose == "interactive"
        and model.resolvedApi == "openai-responses" and provider.template_id != "opencode_zen_go"
    )
    with connection.cursor() as cursor:
        if not selected:
            job_id = f"agent_run.lifecycle:{run_id}"
            cursor.execute(
                "SELECT 1 FROM runtime.runtime_jobs WHERE job_id=%s AND job_kind='agent_run.lifecycle' "
                "AND lease_purpose='execute' AND status='running' AND lease_owner=%s "
                "AND session_id=%s AND idempotency_key=%s AND payload_ref=%s "
                "AND lease_expires_at_ms>(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
                [job_id, lease_owner, run.session_id, f"{job_id}:{digest}", f"record:agent_run:{run_id}"],
            )
            return HttpResponse(status=204) if cursor.fetchone() else JsonResponse(denied, status=409)
        cursor.execute(NATIVE_MODEL_AUTHORITY_SQL, values)
        row = cursor.fetchone()
    if row is None:
        return JsonResponse(denied, status=409)
    stamp = row[0]
    authority = json.loads(stamp)
    try:
        validate_model_endpoint(authority["apiBase"])
    except ModelEndpointValidationError as error:
        return JsonResponse({"error": error.code}, status=409)
    try:
        encrypted_secret = ProviderCredential.objects.values_list("encryptedSecret", flat=True).get(
            id=authority["credentialId"], provider_id=provider_id, version=authority["credentialVersion"],
        )
        token = decrypt_credential_secret(encrypted_secret)
    except ProviderCredential.DoesNotExist:
        return JsonResponse({"error": "provider_secret_unavailable"}, status=409)
    except CredentialDecryptionError:
        return JsonResponse({"error": "provider_secret_decryption_failed"}, status=409)
    grant = {
        "schema": "workspace.native_model_grant.v1",
        "authorityDigest": "sha256:" + hashlib.sha256(stamp.encode("utf-8")).hexdigest(),
        "apiBase": authority["apiBase"],
        "model": authority["model"],
        "maxOutputTokens": authority["maxOutputTokens"],
        "thinkingMode": authority["thinkingMode"],
        "providerTimeoutMs": settings.MODEL_PROVIDER_TIMEOUT_SECONDS * 1000,
        "bearerToken": token,
    }
    return _seal_native_model_grant(grant, proof)


def _seal_native_model_grant(grant, proof):
    aad = proof.encode("utf-8")
    key = hmac.digest(
        settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY.encode("utf-8"),
        b"workspace:native-model-grant:reply:v1\0" + aad,
        "sha256",
    )
    nonce = secrets.token_bytes(12)
    plaintext = json.dumps(grant, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    ciphertext = AESGCM(key).encrypt(nonce, plaintext, aad)
    return JsonResponse({
        "schema": "workspace.native_model_grant.sealed.v1",
        "nonce": urlsafe_b64encode(nonce).rstrip(b"=").decode("ascii"),
        "ciphertext": urlsafe_b64encode(ciphertext).rstrip(b"=").decode("ascii"),
    })


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
        failure = {
            "reasonType": reason,
            "httpStatus": error.httpStatus if isinstance(error, ModelProviderError) else None,
        }
        if isinstance(error, ModelProviderError) and error.retryAfter is not None:
            failure["retryAfter"] = error.retryAfter
        if isinstance(error, ModelProviderError) and reason == "incomplete_output_token_limit":
            failure["truncatedToolCalls"] = error.truncatedToolCalls or []
        yield encode_model_stream_event("error", failure)
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
    bind_agent_run(agent_run_id)
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
        failure = {
            "error": "model_run_failed",
            "reasonType": safe_model_error_reason(error),
            "httpStatus": error.httpStatus if isinstance(error, ModelProviderError) else None,
        }
        if isinstance(error, ModelProviderError) and error.retryAfter is not None:
            failure["retryAfter"] = error.retryAfter
        if isinstance(error, ModelProviderError) and error.reasonType == "incomplete_output_token_limit":
            failure["truncatedToolCalls"] = error.truncatedToolCalls or []
        return JsonResponse(
            failure,
            status=502,
        )
    finally:
        cancel_event.set()
    return JsonResponse(result)


@model_database_call
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
        "agent_run__environment",
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
    run = authorization.agent_run
    from app_core.workspace_resources import models_for_workspace
    if not models_for_workspace(run.workspace_id).filter(pk=run.modelConfig_id).exists():
        return JsonResponse({"error":"model_workspace_rejected"},status=409)
    job_id = f"agent_run.lifecycle:{run.pk}"
    with connection.cursor() as cursor:
        cursor.execute("SELECT 1 FROM runtime.runtime_jobs WHERE job_id=%s "
            "AND job_kind='agent_run.lifecycle' AND lease_purpose='execute' AND status='running' "
            "AND session_id=%s AND idempotency_key=%s "
            "AND lease_expires_at_ms>(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            [job_id, run.session_id, f"{job_id}:{authorization.digest}"])
        if cursor.fetchone() is None:
            return JsonResponse({"error": "model_execution_lease_required"}, status=409)
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
    return authorization.payload["modelConfigRef"]
