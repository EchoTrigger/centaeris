from time import perf_counter
import hashlib
import json

from django.db import IntegrityError, transaction
from django.utils import timezone
from ninja import Router, Status
from ninja.responses import codes_4xx
from pydantic import Field

from app_core.model_adapter import (
    ModelProviderError,
    credential_for_provider,
    encrypt_credential_secret,
    safe_model_error_reason,
    test_model_config,
)
from app_core.models import (
    MODEL_API_IDS,
    AgentRun,
    CredentialAuditEvent,
    ModelConfig,
    ModelEndpointValidationError,
    ModelProvider,
    ProviderCredential,
)
from app_core.runtime_client import request_model_catalog

from .response_schema import (
    AdminModelEnvelope,
    AdminModelsEnvelope,
    COMMON_ERROR_RESPONSES,
    ModelProviderEnvelope,
    ModelProvidersEnvelope,
    ModelTestResponse,
    ModelProviderTemplatesEnvelope,
    ModelCatalogReconciliationEnvelope,
    RunIdsErrorResponse,
)
from .schema import ErrorResponse, StrictSchema
from .security import superuser_auth
from .serialization import (
    serialize_admin_model,
    serialize_model_provider,
)


router = Router(tags=["model-management"], by_alias=True)


def _catalog_templates() -> tuple[dict, dict]:
    templates = {}
    route_overrides = {}
    for provider in request_model_catalog()["providers"]:
        models = []
        for definition in provider["models"]:
            model = {
                "modelName": definition["model"],
                "displayName": definition["displayName"],
                "contextTokens": definition["contextTokens"],
                "maxOutputTokens": definition["maxOutputTokens"],
                "thinkingMode": definition["thinkingMode"],
                "thinkingModes": definition["thinkingModes"],
            }
            if definition["apiOverride"] is not None:
                model["apiOverride"] = definition["apiOverride"]
            if definition["apiBaseOverride"] is not None:
                route_overrides[(provider["catalogId"], definition["model"])] = definition[
                    "apiBaseOverride"
                ]
            models.append(model)
        templates[provider["catalogId"]] = {
            "id": provider["catalogId"],
            "displayName": provider["displayName"],
            "tier": provider.get("tier", "direct_api"),
            "logoSvg": provider.get("logoSvg"),
            "api": provider["api"],
            "apiBase": provider["apiBase"],
            "models": models,
        }
    return templates, route_overrides


class CreateModelProviderRequest(StrictSchema):
    display_name: str = Field(alias="displayName")
    api: str
    api_base: str = Field(alias="apiBase")
    secret: str


class UpdateModelProviderRequest(StrictSchema):
    display_name: str = Field(default=None, alias="displayName")
    api: str = None
    api_base: str = Field(default=None, alias="apiBase")
    enabled: bool = None


class RotateProviderCredentialRequest(StrictSchema):
    secret: str


class CreateModelRequest(StrictSchema):
    display_name: str = Field(default="", alias="displayName")
    provider_id: str = Field(alias="providerId")
    model_name: str = Field(alias="modelName")
    api_override: str | None = Field(default=None, alias="apiOverride")
    context_tokens: int = Field(alias="contextTokens")
    max_output_tokens: int = Field(alias="maxOutputTokens")
    thinking_mode: str | None = Field(default=None, alias="thinkingMode")
    thinking_modes: list[str] = Field(default_factory=list, alias="thinkingModes")
    enabled: bool


class UpdateModelRequest(StrictSchema):
    display_name: str = Field(default=None, alias="displayName")
    provider_id: str = Field(default=None, alias="providerId")
    model_name: str = Field(default=None, alias="modelName")
    api_override: str | None = Field(default=None, alias="apiOverride")
    context_tokens: int = Field(default=None, alias="contextTokens")
    max_output_tokens: int = Field(default=None, alias="maxOutputTokens")
    thinking_mode: str | None = Field(default=None, alias="thinkingMode")
    thinking_modes: list[str] | None = Field(default=None, alias="thinkingModes")
    enabled: bool = None


class EmptyRequest(StrictSchema):
    pass


class ApplyModelCatalogReconciliationRequest(StrictSchema):
    expected_digest: str = Field(alias="expectedDigest")


def _model_catalog_reconciliation(templates: dict, route_overrides: dict, *, lock: bool = False) -> dict:
    providers_query = ModelProvider.objects.filter(
        template_id__isnull=False, archivedAt__isnull=True
    ).order_by("id")
    if lock:
        providers_query = providers_query.select_for_update()
    entries = []
    for provider in providers_query:
        template = templates.get(provider.template_id)
        if template is None:
            entries.append({"providerId": provider.id, "templateId": provider.template_id,
                            "changes": [], "blockers": ["template_removed"]})
            continue
        current_query = ModelConfig.objects.filter(provider=provider, isCurrent=True).order_by("modelName")
        if lock:
            current_query = current_query.select_for_update()
        current = {model.modelName: model for model in current_query}
        target = {model["modelName"]: model for model in template["models"]}
        blockers = []
        changes = []
        if provider.api != template["api"] or provider.apiBase != template["apiBase"]:
            changes.append({"action": "update_provider_route", "from": {
                "api": provider.api, "apiBase": provider.apiBase,
            }, "to": {"api": template["api"], "apiBase": template["apiBase"]}})
        if provider.displayName != template["displayName"]:
            changes.append({"action": "rename_provider", "from": provider.displayName,
                            "to": template["displayName"]})
        for name, model in current.items():
            expected = target.get(name)
            if expected is None:
                active_ids = list(AgentRun.objects.filter(
                    modelConfig=model, status__in=["queued", "running"]
                ).order_by("id").values_list("id", flat=True))
                changes.append({"action": "retire", "modelName": name, "modelId": model.id,
                                "activeAgentRunIds": active_ids})
                continue
            desired = (
                expected["displayName"], expected.get("apiOverride"),
                expected.get("apiOverride") or template["api"],
                expected["contextTokens"], expected["maxOutputTokens"],
                expected.get("thinkingMode") or "", expected.get("thinkingModes", []),
                route_overrides.get((provider.template_id, name)) or template["apiBase"],
            )
            existing = (
                model.displayName, model.apiOverride, model.resolvedApi, model.contextTokens,
                model.maxOutputTokens, model.thinkingMode, model.thinkingModes,
                model.resolvedApiBase,
            )
            if desired != existing:
                changes.append({"action": "revise", "modelName": name,
                                "modelId": model.id, "revision": model.revision + 1})
        for name in target.keys() - current.keys():
            changes.append({"action": "add", "modelName": name})
        changes.sort(key=lambda change: (change.get("modelName", ""), change["action"]))
        entries.append({"providerId": provider.id, "templateId": provider.template_id,
                        "changes": changes, "blockers": sorted(set(blockers))})
    catalog_projection = {
        template_id: {
            "displayName": template["displayName"], "api": template["api"],
            "apiBase": template["apiBase"], "models": template["models"],
        }
        for template_id, template in templates.items()
    }
    digest_entries = [
        {**entry, "changes": [
            {key: value for key, value in change.items() if key != "activeAgentRunIds"}
            for change in entry["changes"]
        ]}
        for entry in entries
    ]
    digest_input = {"entries": digest_entries, "catalog": catalog_projection,
                    "routeOverrides": sorted((list(key) + [base]) for key, base in route_overrides.items())}
    digest = hashlib.sha256(json.dumps(digest_input, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    return {"digest": digest, "providers": entries,
            "blocked": any(entry["blockers"] for entry in entries)}


@router.get(
    "/admin/model-catalog-reconciliation",
    auth=superuser_auth,
    response={200: ModelCatalogReconciliationEnvelope} | COMMON_ERROR_RESPONSES,
)
def preview_model_catalog_reconciliation(request):
    templates, route_overrides = _catalog_templates()
    return _model_catalog_reconciliation(templates, route_overrides)


@router.post(
    "/admin/model-catalog-reconciliation",
    auth=superuser_auth,
    response={200: ModelCatalogReconciliationEnvelope} | COMMON_ERROR_RESPONSES,
)
def apply_model_catalog_reconciliation(request, payload: ApplyModelCatalogReconciliationRequest):
    templates, route_overrides = _catalog_templates()
    with transaction.atomic():
        preview = _model_catalog_reconciliation(templates, route_overrides, lock=True)
        if preview["digest"] != payload.expected_digest:
            return Status(409, {"error": "model_catalog_reconciliation_stale"})
        if preview["blocked"]:
            return Status(409, {"error": "model_catalog_reconciliation_blocked"})
        for entry in preview["providers"]:
            template = templates[entry["templateId"]]
            provider = ModelProvider.objects.get(id=entry["providerId"])
            for change in entry["changes"]:
                action = change["action"]
                if action == "rename_provider":
                    provider.displayName = template["displayName"]
                    provider.save(update_fields=["displayName", "updatedAt"])
                    continue
                if action == "update_provider_route":
                    provider.api = template["api"]
                    provider.apiBase = template["apiBase"]
                    provider.save(update_fields=["api", "apiBase", "updatedAt"])
                    continue
                current = None
                was_enabled = True
                if action in {"retire", "revise"}:
                    current = ModelConfig.objects.get(id=change["modelId"], isCurrent=True)
                    was_enabled = current.enabled
                    current.isCurrent = False
                    current.enabled = False
                    current.save(update_fields=["isCurrent", "enabled", "updatedAt"])
                if action == "retire":
                    continue
                values = next(model for model in template["models"] if model["modelName"] == change["modelName"])
                _new_model_config(
                    provider=provider, display_name=values["displayName"],
                    model_name=values["modelName"], api_override=values.get("apiOverride"),
                    resolved_api_base=route_overrides.get((entry["templateId"], values["modelName"])),
                    context_tokens=values["contextTokens"], max_output_tokens=values["maxOutputTokens"],
                    thinking_mode=values.get("thinkingMode"), thinking_modes=values.get("thinkingModes", []),
                    enabled=was_enabled,
                    family_id=current.familyId if current is not None else None,
                    revision=current.revision + 1 if current is not None else 1,
                ).save()
    return _model_catalog_reconciliation(templates, route_overrides)


@router.get(
    "/admin/model-providers",
    auth=superuser_auth,
    response={200: ModelProvidersEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_model_providers(request):
    providers = ModelProvider.objects.filter(archivedAt__isnull=True).order_by("displayName", "id")
    return {"providers": [serialize_model_provider(provider) for provider in providers]}


@router.get(
    "/admin/model-provider-templates",
    auth=superuser_auth,
    response={200: ModelProviderTemplatesEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_model_provider_templates(request):
    templates, _ = _catalog_templates()
    return {"templates": list(templates.values())}


@router.post(
    "/admin/model-provider-templates/{template_id}/instantiate",
    auth=superuser_auth,
    response={201: ModelProviderEnvelope} | COMMON_ERROR_RESPONSES,
)
def instantiate_model_provider_template(
    request,
    template_id: str,
    payload: RotateProviderCredentialRequest,
):
    templates, route_overrides = _catalog_templates()
    template = templates.get(template_id)
    if template is None:
        return Status(404, {"error": "model_provider_template_not_found"})
    try:
        secret = _validated_secret(payload.secret)
        with transaction.atomic():
            provider = _create_provider(
                request,
                display_name=template["displayName"],
                template_id=template_id,
                api=template["api"],
                api_base=template["apiBase"],
                secret=secret,
            )
            for values in template["models"]:
                _new_model_config(
                    provider=provider,
                    display_name=values["displayName"],
                    model_name=values["modelName"],
                    api_override=values.get("apiOverride"),
                    resolved_api_base=route_overrides.get(
                        (template_id, values["modelName"])
                    ),
                    context_tokens=values["contextTokens"],
                    max_output_tokens=values["maxOutputTokens"],
                    thinking_mode=values.get("thinkingMode"),
                    thinking_modes=values.get("thinkingModes", []),
                    enabled=True,
                ).save()
    except IntegrityError:
        return Status(409, {"error": "model_provider_conflict"})
    except ModelEndpointValidationError as error:
        return Status(400, {"error": error.code})
    except (TypeError, ValueError):
        return Status(400, {"error": "model_provider_invalid"})
    return Status(201, {"provider": serialize_model_provider(provider)})


@router.post(
    "/admin/model-providers",
    auth=superuser_auth,
    response={201: ModelProviderEnvelope} | COMMON_ERROR_RESPONSES,
)
def create_model_provider(request, payload: CreateModelProviderRequest):
    try:
        secret = _validated_secret(payload.secret)
        with transaction.atomic():
            provider = _create_provider(
                request,
                display_name=payload.display_name,
                template_id=None,
                api=payload.api,
                api_base=payload.api_base,
                secret=secret,
            )
    except IntegrityError:
        return Status(409, {"error": "model_provider_conflict"})
    except ModelEndpointValidationError as error:
        return Status(400, {"error": error.code})
    except (TypeError, ValueError):
        return Status(400, {"error": "model_provider_invalid"})
    return Status(201, {"provider": serialize_model_provider(provider)})


@router.patch(
    "/admin/model-providers/{provider_id}",
    auth=superuser_auth,
    response={200: ModelProviderEnvelope} | COMMON_ERROR_RESPONSES,
)
def update_model_provider(request, provider_id: str, payload: UpdateModelProviderRequest):
    try:
        if not payload.model_fields_set:
            raise ValueError
        with transaction.atomic():
            provider = ModelProvider.objects.select_for_update().get(
                id=provider_id,
                archivedAt__isnull=True,
            )
            if provider.template_id is not None and payload.model_fields_set != {"enabled"}:
                return Status(400, {"error": "preset_model_provider_read_only"})
            old_route = (provider.api, provider.apiBase)
            if "display_name" in payload.model_fields_set:
                provider.displayName = payload.display_name.strip()
            if "api" in payload.model_fields_set:
                provider.api = payload.api.strip()
            if "api_base" in payload.model_fields_set:
                provider.apiBase = payload.api_base.strip()
            if "enabled" in payload.model_fields_set:
                provider.enabled = payload.enabled
            provider.save()
            credential_for_provider(provider, lock=True)
            if old_route != (provider.api, provider.apiBase):
                _replace_provider_model_revisions(provider)
    except ModelProvider.DoesNotExist:
        return Status(404, {"error": "model_provider_not_found"})
    except ModelProviderError:
        return Status(400, {"error": "provider_secret_unavailable"})
    except ModelEndpointValidationError as error:
        return Status(400, {"error": error.code})
    except (TypeError, ValueError):
        return Status(400, {"error": "model_provider_invalid"})
    return {"provider": serialize_model_provider(provider)}


@router.post(
    "/admin/model-providers/{provider_id}/credential/rotate",
    auth=superuser_auth,
    response={200: ModelProviderEnvelope} | COMMON_ERROR_RESPONSES,
)
def rotate_model_provider_credential(
    request,
    provider_id: str,
    payload: RotateProviderCredentialRequest,
):
    try:
        secret = _validated_secret(payload.secret)
        with transaction.atomic():
            provider = ModelProvider.objects.select_for_update().get(
                id=provider_id,
                archivedAt__isnull=True,
            )
            credential = credential_for_provider(provider, lock=True)
            credential.encryptedSecret = encrypt_credential_secret(secret)
            credential.version += 1
            credential.updatedBy = request.user
            credential.save(update_fields=["encryptedSecret", "version", "updatedBy", "updatedAt"])
            _record_credential_audit(credential, "rotated", request.user)
    except ModelProvider.DoesNotExist:
        return Status(404, {"error": "model_provider_not_found"})
    except ModelProviderError:
        return Status(400, {"error": "provider_secret_unavailable"})
    except (TypeError, ValueError):
        return Status(400, {"error": "credential_invalid"})
    return {"provider": serialize_model_provider(provider)}


@router.delete(
    "/admin/model-providers/{provider_id}",
    auth=superuser_auth,
    response={204: None, codes_4xx: ErrorResponse | RunIdsErrorResponse},
)
def delete_model_provider(request, provider_id: str):
    with transaction.atomic():
        try:
            provider = ModelProvider.objects.select_for_update().get(
                id=provider_id,
                archivedAt__isnull=True,
            )
        except ModelProvider.DoesNotExist:
            return Status(404, {"error": "model_provider_not_found"})
        active_agent_run_ids = list(
            AgentRun.objects.filter(
                modelConfig__provider=provider,
                status__in=["queued", "running"],
            ).values_list("id", flat=True)
        )
        if active_agent_run_ids:
            return Status(409, {"error": "model_provider_has_active_agent_runs", "agentRunIds": active_agent_run_ids})
        now = timezone.now()
        provider.enabled = False
        provider.archivedAt = now
        provider.save(update_fields=["enabled", "archivedAt", "updatedAt"])
        ModelConfig.objects.filter(provider=provider, isCurrent=True).update(
            enabled=False,
            isCurrent=False,
            updatedAt=now,
        )
    return Status(204, None)


@router.get(
    "/admin/models",
    auth=superuser_auth,
    response={200: AdminModelsEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_models(request):
    models = ModelConfig.objects.select_related("provider").filter(isCurrent=True, provider__archivedAt__isnull=True).order_by("displayName", "id")
    return {"models": [serialize_admin_model(model) for model in models]}


@router.post(
    "/admin/models",
    auth=superuser_auth,
    response={201: AdminModelEnvelope} | COMMON_ERROR_RESPONSES,
)
def create_model(request, payload: CreateModelRequest):
    try:
        with transaction.atomic():
            provider = _active_provider(payload.provider_id)
            if provider.template_id is not None:
                return Status(400, {"error": "preset_provider_models_read_only"})
            credential_for_provider(provider, lock=True)
            model = _new_model_config(
                provider=provider,
                display_name=payload.display_name,
                model_name=payload.model_name,
                api_override=payload.api_override,
                context_tokens=payload.context_tokens,
                max_output_tokens=payload.max_output_tokens,
                thinking_mode=payload.thinking_mode,
                thinking_modes=payload.thinking_modes,
                enabled=payload.enabled,
            )
            model.save()
    except ModelProvider.DoesNotExist:
        return Status(400, {"error": "model_provider_unavailable"})
    except ModelProviderError:
        return Status(400, {"error": "provider_secret_unavailable"})
    except ModelEndpointValidationError as error:
        return Status(400, {"error": error.code})
    except IntegrityError:
        return Status(409, {"error": "model_provider_model_conflict"})
    except (TypeError, ValueError):
        return Status(400, {"error": "model_config_invalid"})
    return Status(201, {"model": serialize_admin_model(model)})


@router.patch(
    "/admin/models/{model_id}",
    auth=superuser_auth,
    response={200: AdminModelEnvelope} | COMMON_ERROR_RESPONSES,
)
def update_model(request, model_id: str, payload: UpdateModelRequest):
    try:
        if not payload.model_fields_set:
            raise ValueError
        with transaction.atomic():
            current = ModelConfig.objects.select_for_update().get(id=model_id, isCurrent=True)
            provider_id = (
                payload.provider_id.strip()
                if "provider_id" in payload.model_fields_set
                else current.provider_id
            )
            provider = _active_provider(provider_id)
            if provider.template_id is not None:
                return Status(400, {"error": "preset_provider_models_read_only"})
            credential_for_provider(provider, lock=True)
            model = _new_model_config(
                provider=provider,
                display_name=(payload.display_name if "display_name" in payload.model_fields_set else current.displayName),
                model_name=(payload.model_name if "model_name" in payload.model_fields_set else current.modelName),
                api_override=(payload.api_override if "api_override" in payload.model_fields_set else current.apiOverride),
                context_tokens=(payload.context_tokens if "context_tokens" in payload.model_fields_set else current.contextTokens),
                max_output_tokens=(payload.max_output_tokens if "max_output_tokens" in payload.model_fields_set else current.maxOutputTokens),
                thinking_mode=(payload.thinking_mode if "thinking_mode" in payload.model_fields_set else current.thinkingMode),
                thinking_modes=(payload.thinking_modes if "thinking_modes" in payload.model_fields_set else current.thinkingModes),
                enabled=(payload.enabled if "enabled" in payload.model_fields_set else current.enabled),
                family_id=current.familyId,
                revision=current.revision + 1,
            )
            current.isCurrent = False
            current.enabled = False
            current.save(update_fields=["isCurrent", "enabled", "updatedAt"])
            model.save()
    except ModelConfig.DoesNotExist:
        return Status(404, {"error": "model_not_found"})
    except ModelProvider.DoesNotExist:
        return Status(400, {"error": "model_provider_unavailable"})
    except ModelProviderError:
        return Status(400, {"error": "provider_secret_unavailable"})
    except ModelEndpointValidationError as error:
        return Status(400, {"error": error.code})
    except IntegrityError:
        return Status(409, {"error": "model_provider_model_conflict"})
    except (TypeError, ValueError):
        return Status(400, {"error": "model_config_invalid"})
    return {"model": serialize_admin_model(model)}


@router.delete(
    "/admin/models/{model_id}",
    auth=superuser_auth,
    response={204: None, codes_4xx: ErrorResponse | RunIdsErrorResponse},
)
def delete_model(request, model_id: str):
    with transaction.atomic():
        try:
            model = ModelConfig.objects.select_for_update().get(id=model_id, isCurrent=True)
        except ModelConfig.DoesNotExist:
            return Status(404, {"error": "model_not_found"})
        if model.provider.template_id is not None:
            return Status(400, {"error": "preset_provider_models_read_only"})
        model.isCurrent = False
        model.enabled = False
        model.save(update_fields=["isCurrent", "enabled", "updatedAt"])
    return Status(204, None)


@router.post(
    "/admin/models/{model_id}/test",
    auth=superuser_auth,
    response={200: ModelTestResponse, codes_4xx: ErrorResponse},
)
def test_model(request, model_id: str, payload: EmptyRequest | None = None):
    started_at = perf_counter()
    try:
        model = ModelConfig.objects.select_related("provider").get(id=model_id, isCurrent=True)
        credential = credential_for_provider(model.provider)
        result = test_model_config(model)
    except ModelConfig.DoesNotExist:
        return Status(404, {"error": "model_not_found"})
    except ModelEndpointValidationError as error:
        return Status(400, {"error": error.code})
    except ModelProviderError as error:
        if error.reasonType in {"model_endpoint_https_required", "provider_unavailable"}:
            return Status(400, {"error": error.reasonType})
        return _test_result(error=error, started_at=started_at)
    except (TypeError, ValueError):
        return Status(400, {"error": "invalid_json"})
    except Exception as error:
        return _test_result(error=ModelProviderError(safe_model_error_reason(error)), started_at=started_at)
    _record_credential_audit(credential, "tested", request.user)
    return _test_result(result=result, started_at=started_at)


def _validated_secret(secret: str) -> str:
    if not isinstance(secret, str) or not secret.strip() or len(secret) > 4096:
        raise ValueError("credential secret is required")
    return secret


def _create_provider(
    request,
    *,
    display_name: str,
    template_id: str | None,
    api: str,
    api_base: str,
    secret: str,
) -> ModelProvider:
    provider = ModelProvider(
        displayName=display_name.strip(),
        template_id=template_id,
        api=api.strip(),
        apiBase=api_base.strip(),
    )
    provider.save()
    credential = ProviderCredential.objects.create(
        provider=provider,
        displayName=provider.displayName,
        encryptedSecret=encrypt_credential_secret(secret),
        createdBy=request.user,
        updatedBy=request.user,
    )
    _record_credential_audit(credential, "created", request.user)
    return provider


def _test_result(*, started_at: float, result: dict | None = None, error: ModelProviderError | None = None) -> dict:
    latency_ms = max(0, round((perf_counter() - started_at) * 1000))
    if error is not None:
        return {
            "ok": False,
            "httpStatus": error.httpStatus,
            "latencyMs": latency_ms,
            "outputPreview": None,
            "errorKeyword": error.reasonType,
        }
    output = result["text"] if isinstance(result, dict) else ""
    output_preview = " ".join(output.split())[:96] if isinstance(output, str) else ""
    return {
        "ok": True,
        "httpStatus": 200,
        "latencyMs": latency_ms,
        "outputPreview": output_preview or None,
        "errorKeyword": None,
    }


def _active_provider(provider_id: str) -> ModelProvider:
    return ModelProvider.objects.select_for_update().get(
        id=provider_id.strip(),
        enabled=True,
        archivedAt__isnull=True,
    )


def _new_model_config(
    *,
    provider: ModelProvider,
    display_name: str,
    model_name: str,
    api_override: str | None,
    context_tokens: int,
    max_output_tokens: int,
    thinking_mode: str | None,
    thinking_modes: list[str],
    enabled: bool,
    resolved_api_base: str | None = None,
    family_id: str | None = None,
    revision: int = 1,
) -> ModelConfig:
    api_override = api_override.strip() if isinstance(api_override, str) else None
    if api_override not in {None, *MODEL_API_IDS}:
        raise ValueError("apiOverride is unsupported")
    values = {
        "provider": provider,
        "displayName": display_name.strip(),
        "modelName": model_name.strip(),
        "apiOverride": api_override,
        "resolvedApi": api_override or provider.api,
        "resolvedApiBase": resolved_api_base or provider.apiBase,
        "contextTokens": context_tokens,
        "maxOutputTokens": max_output_tokens,
        "thinkingMode": thinking_mode or "",
        "thinkingModes": thinking_modes,
        "enabled": enabled,
        "revision": revision,
    }
    if family_id is not None:
        values["familyId"] = family_id
    return ModelConfig(**values)


def _replace_provider_model_revisions(provider: ModelProvider) -> None:
    current_models = list(
        ModelConfig.objects.select_for_update().filter(provider=provider, isCurrent=True)
    )
    for current in current_models:
        replacement = _new_model_config(
            provider=provider,
            display_name=current.displayName,
            model_name=current.modelName,
            api_override=current.apiOverride,
            context_tokens=current.contextTokens,
            max_output_tokens=current.maxOutputTokens,
            thinking_mode=current.thinkingMode,
            thinking_modes=current.thinkingModes,
            enabled=current.enabled,
            family_id=current.familyId,
            revision=current.revision + 1,
        )
        current.isCurrent = False
        current.enabled = False
        current.save(update_fields=["isCurrent", "enabled", "updatedAt"])
        replacement.save()


def _record_credential_audit(credential: ProviderCredential, action: str, actor) -> None:
    CredentialAuditEvent.objects.create(
        credentialId=credential.id,
        provider=credential.provider_id,
        displayName=credential.displayName,
        action=action,
        actor=actor,
    )
