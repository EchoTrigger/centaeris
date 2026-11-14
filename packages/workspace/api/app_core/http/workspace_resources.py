"""Administration of workspace resources."""

from types import SimpleNamespace
from typing import Literal
from django.db import IntegrityError, transaction
from django.http import JsonResponse, HttpResponse
from ninja import Router, Status
from ninja.responses import codes_4xx
from app_core.models import Workspace, ModelProvider, ModelConfig, McpBearerCredential
from app_core.workspace_resources import workspace_resource_admin, resource_scope
from app_core.plugin_catalog import load_plugin_bearer_credential_refs
from app_core.credentials import (
    encrypt_credential_secret,
    normalize_bearer_token_input,
    validate_display_name,
    validate_lower_kebab,
)
from .security import session_auth
from . import model_management as mm
from . import mcp_credentials as mc
from .response_schema import (
    AdminModelResponse,
    COMMON_ERROR_RESPONSES,
    ModelProviderResponse,
    ModelProviderTemplatesEnvelope,
    ModelTestResponse,
    RunIdsErrorResponse,
    WorkspacePluginEnvelope,
)
from .schema import ErrorResponse, StrictSchema
from .serialization import serialize_model_provider, serialize_admin_model



router = Router(tags=["workspace resources"], by_alias=True)
plugin_upload_router = Router(tags=["workspace resources"], by_alias=True)
class ResourceScopeResponse(StrictSchema):
    scope: Literal["workspace", "platform"]
    writable: bool


class ScopedModelProviderResponse(ModelProviderResponse, ResourceScopeResponse):
    pass


class ScopedModelProviderEnvelope(StrictSchema):
    provider: ScopedModelProviderResponse


class ScopedModelProvidersEnvelope(StrictSchema):
    providers: list[ScopedModelProviderResponse]


class ScopedAdminModelResponse(AdminModelResponse, ResourceScopeResponse):
    pass


class ScopedAdminModelEnvelope(StrictSchema):
    model: ScopedAdminModelResponse


class ScopedAdminModelsEnvelope(StrictSchema):
    models: list[ScopedAdminModelResponse]


class ScopedMcpBearerCredentialResponse(mc.McpBearerCredentialResponse, ResourceScopeResponse):
    pass


class ScopedMcpBearerCredentialEnvelope(StrictSchema):
    credential: ScopedMcpBearerCredentialResponse


class ScopedMcpBearerCredentialsEnvelope(StrictSchema):
    credentials: list[ScopedMcpBearerCredentialResponse]


def _response(result):
    status = result.status_code if isinstance(result, Status) else 200
    value = result.value if isinstance(result, Status) else result
    return (
        HttpResponse(status=204)
        if status == 204
        else JsonResponse(value, status=status)
    )


def model_action(request, workspace_id, action, **kwargs):
    with transaction.atomic():
        try:
            workspace = workspace_resource_admin(request.user, workspace_id)
        except PermissionError:
            return JsonResponse({"error": "workspace_not_found"}, status=404)
        # This adapter is created after authorization and cannot be supplied by JSON.
        scoped = SimpleNamespace(user=request.user, resource_workspace=workspace)
        result = action(scoped, **kwargs)
        value = result.value if isinstance(result, Status) else result
        if isinstance(value, dict):
            for field in ("provider", "model"):
                if isinstance(value.get(field), dict):
                    value[field].update(resource_scope(workspace.id))
        return _response(result)


@router.get(
    "/workspaces/{workspace_id}/model-providers", auth=session_auth, response={200: ScopedModelProvidersEnvelope} | COMMON_ERROR_RESPONSES
)
def providers(request, workspace_id: str):
    with transaction.atomic():
        try:
            workspace_resource_admin(request.user, workspace_id)
        except PermissionError:
            return JsonResponse({"error": "workspace_not_found"}, status=404)
        items = ModelProvider.objects.filter(
            workspace_id=workspace_id,
            archivedAt__isnull=True,
        ).order_by("displayName", "id")
        return JsonResponse(
            {
                "providers": [
                    serialize_model_provider(x) | resource_scope(x.workspace_id)
                    for x in items
                ]
            }
        )


@router.get("/workspaces/{workspace_id}/models", auth=session_auth, response={200: ScopedAdminModelsEnvelope} | COMMON_ERROR_RESPONSES)
def models(request, workspace_id: str):
    with transaction.atomic():
        try:
            workspace_resource_admin(request.user, workspace_id)
        except PermissionError:
            return JsonResponse({"error": "workspace_not_found"}, status=404)
        items = (
            ModelConfig.objects.select_related("provider")
            .filter(
                provider__workspace_id=workspace_id,
                isCurrent=True,
                provider__archivedAt__isnull=True,
            )
            .order_by("displayName", "id")
        )
        return JsonResponse(
            {
                "models": [
                    serialize_admin_model(x)
                    | resource_scope(x.provider.workspace_id)
                    for x in items
                ]
            }
        )


def credential_ref_exists(workspace, plugin_name, credential_ref):
    from app_core.workspace_plugins import workspace_plugin_catalog

    package = next(
        (
            p
            for p in workspace_plugin_catalog(workspace)["packages"]
            if p["name"] == plugin_name
        ),
        None,
    )
    return package is not None and credential_ref in load_plugin_bearer_credential_refs(
        package
    )


@router.get(
    "/workspaces/{workspace_id}/mcp-bearer-credentials",
    auth=session_auth,
    response={200: ScopedMcpBearerCredentialsEnvelope} | COMMON_ERROR_RESPONSES,
)
def credentials(request, workspace_id: str):
    with transaction.atomic():
        try:
            workspace_resource_admin(request.user, workspace_id)
        except PermissionError:
            return JsonResponse({"error": "workspace_not_found"}, status=404)
        items = McpBearerCredential.objects.filter(
            workspace_id=workspace_id
        ).order_by("plugin_name", "credential_ref", "workspace_id")
        return JsonResponse(
            {
                "credentials": [
                    mc._serialize_credential(x) | resource_scope(x.workspace_id)
                    for x in items
                ]
            }
        )


@router.post(
    "/workspaces/{workspace_id}/mcp-bearer-credentials",
    auth=session_auth,
    response={201: ScopedMcpBearerCredentialEnvelope} | COMMON_ERROR_RESPONSES,
)
def create_credential(
    request, workspace_id: str, payload: mc.McpBearerCredentialRequest
):
    try:
        with transaction.atomic():
            workspace = workspace_resource_admin(request.user, workspace_id)
            plugin = validate_lower_kebab("plugin", payload.plugin_name)
            ref = validate_lower_kebab("credential ref", payload.credential_ref)
            if not credential_ref_exists(workspace, plugin, ref):
                return JsonResponse(
                    {"error": "mcp_credential_ref_not_found"}, status=404
                )
            credential = McpBearerCredential.objects.create(
                workspace=workspace,
                plugin_name=plugin,
                credential_ref=ref,
                display_name=validate_display_name(payload.display_name),
                encrypted_secret=encrypt_credential_secret(
                    normalize_bearer_token_input(payload.secret)
                ),
                created_by=request.user,
                updated_by=request.user,
            )
            mc._record_audit(credential, "created", request.user)
            return JsonResponse(
                {
                    "credential": mc._serialize_credential(credential)
                    | resource_scope(workspace.id)
                },
                status=201,
            )
    except PermissionError:
        return JsonResponse({"error": "workspace_not_found"}, status=404)
    except IntegrityError:
        return JsonResponse({"error": "mcp_bearer_credential_conflict"}, status=409)
    except ValueError:
        return JsonResponse({"error": "mcp_bearer_credential_invalid"}, status=400)


@router.post(
    "/workspaces/{workspace_id}/mcp-bearer-credentials/{credential_id}/rotate",
    auth=session_auth,
    response={200: ScopedMcpBearerCredentialEnvelope} | COMMON_ERROR_RESPONSES,
)
def rotate_credential(
    request,
    workspace_id: str,
    credential_id: str,
    payload: mc.RotateMcpBearerCredentialRequest,
):
    try:
        with transaction.atomic():
            workspace_resource_admin(request.user, workspace_id)
            credential = McpBearerCredential.objects.select_for_update().get(
                pk=credential_id, workspace_id=workspace_id
            )
            credential.encrypted_secret = encrypt_credential_secret(
                normalize_bearer_token_input(payload.secret)
            )
            credential.version += 1
            credential.updated_by = request.user
            credential.save(
                update_fields=[
                    "encrypted_secret",
                    "version",
                    "updated_by",
                    "updated_at",
                ]
            )
            mc._record_audit(credential, "rotated", request.user)
            return JsonResponse(
                {
                    "credential": mc._serialize_credential(credential)
                    | resource_scope(workspace_id)
                }
            )
    except PermissionError:
        return JsonResponse({"error": "workspace_not_found"}, status=404)
    except McpBearerCredential.DoesNotExist:
        return JsonResponse({"error": "mcp_bearer_credential_not_found"}, status=404)
    except ValueError:
        return JsonResponse({"error": "mcp_bearer_credential_invalid"}, status=400)


@router.delete(
    "/workspaces/{workspace_id}/mcp-bearer-credentials/{credential_id}",
    auth=session_auth,
    response={204: None} | COMMON_ERROR_RESPONSES,
)
def remove_credential(request, workspace_id: str, credential_id: str):
    try:
        with transaction.atomic():
            workspace_resource_admin(request.user, workspace_id)
            credential = McpBearerCredential.objects.select_for_update().get(
                pk=credential_id, workspace_id=workspace_id
            )
            mc._record_audit(credential, "deleted", request.user)
            credential.delete()
            return HttpResponse(status=204)
    except PermissionError:
        return JsonResponse({"error": "workspace_not_found"}, status=404)
    except McpBearerCredential.DoesNotExist:
        return JsonResponse({"error": "mcp_bearer_credential_not_found"}, status=404)


@router.get(
    "/workspaces/{workspace_id}/model-provider-templates",
    auth=session_auth,
    response={200: ModelProviderTemplatesEnvelope} | COMMON_ERROR_RESPONSES,
)
def provider_templates(request, workspace_id: str):
    return model_action(request, workspace_id, mm.list_model_provider_templates)


@router.post(
    "/workspaces/{workspace_id}/model-provider-templates/{template_id}/instantiate",
    auth=session_auth,
    response={201: ScopedModelProviderEnvelope} | COMMON_ERROR_RESPONSES,
)
def instantiate_provider(
    request,
    workspace_id: str,
    template_id: str,
    payload: mm.RotateProviderCredentialRequest,
):
    return model_action(
        request,
        workspace_id,
        mm.instantiate_model_provider_template,
        template_id=template_id,
        payload=payload,
    )


@router.post(
    "/workspaces/{workspace_id}/model-providers", auth=session_auth, response={201: ScopedModelProviderEnvelope} | COMMON_ERROR_RESPONSES
)
def create_provider(request, workspace_id: str, payload: mm.CreateModelProviderRequest):
    return model_action(
        request, workspace_id, mm.create_model_provider, payload=payload
    )


@router.patch(
    "/workspaces/{workspace_id}/model-providers/{provider_id}",
    auth=session_auth,
    response={200: ScopedModelProviderEnvelope} | COMMON_ERROR_RESPONSES,
)
def update_provider(
    request, workspace_id: str, provider_id: str, payload: mm.UpdateModelProviderRequest
):
    return model_action(
        request,
        workspace_id,
        mm.update_model_provider,
        provider_id=provider_id,
        payload=payload,
    )


@router.delete(
    "/workspaces/{workspace_id}/model-providers/{provider_id}",
    auth=session_auth,
    response={204: None} | COMMON_ERROR_RESPONSES | {codes_4xx: ErrorResponse | RunIdsErrorResponse},
)
def delete_provider(request, workspace_id: str, provider_id: str):
    return model_action(
        request, workspace_id, mm.delete_model_provider, provider_id=provider_id
    )


@router.post(
    "/workspaces/{workspace_id}/model-providers/{provider_id}/credential/rotate",
    auth=session_auth,
    response={200: ScopedModelProviderEnvelope} | COMMON_ERROR_RESPONSES,
)
def rotate_provider(
    request,
    workspace_id: str,
    provider_id: str,
    payload: mm.RotateProviderCredentialRequest,
):
    return model_action(
        request,
        workspace_id,
        mm.rotate_model_provider_credential,
        provider_id=provider_id,
        payload=payload,
    )


@router.post("/workspaces/{workspace_id}/models", auth=session_auth, response={201: ScopedAdminModelEnvelope} | COMMON_ERROR_RESPONSES)
def create_model(request, workspace_id: str, payload: mm.CreateModelRequest):
    return model_action(request, workspace_id, mm.create_model, payload=payload)


@router.patch(
    "/workspaces/{workspace_id}/models/{model_id}", auth=session_auth, response={200: ScopedAdminModelEnvelope} | COMMON_ERROR_RESPONSES
)
def update_model(
    request, workspace_id: str, model_id: str, payload: mm.UpdateModelRequest
):
    return model_action(
        request, workspace_id, mm.update_model, model_id=model_id, payload=payload
    )


@router.delete(
    "/workspaces/{workspace_id}/models/{model_id}", auth=session_auth, response={204: None} | COMMON_ERROR_RESPONSES
)
def delete_model(request, workspace_id: str, model_id: str):
    return model_action(request, workspace_id, mm.delete_model, model_id=model_id)


@router.post(
    "/workspaces/{workspace_id}/models/{model_id}/test",
    auth=session_auth,
    response={200: ModelTestResponse} | COMMON_ERROR_RESPONSES,
)
def test_model(
    request, workspace_id: str, model_id: str, payload: mm.EmptyRequest | None = None
):
    return model_action(
        request, workspace_id, mm.test_model, model_id=model_id, payload=payload
    )


@plugin_upload_router.post(
    "/workspaces/{workspace_id}/plugins/upload", auth=session_auth, response={200: WorkspacePluginEnvelope, 413: ErrorResponse} | COMMON_ERROR_RESPONSES
)
def upload_plugin(request, workspace_id: str):
    from app_core.workspace_plugins import install_workspace_plugin
    from app_core.plugin_install_source import (
        UploadedWorkspaceZip,
        PluginInstallSourceError,
    )
    from app_core.plugin_lifecycle import PluginLifecycleError

    if (
        request.GET
        or request.POST
        or set(request.FILES) != {"file"}
        or len(request.FILES.getlist("file")) != 1
    ):
        return JsonResponse({"error": "plugin_lifecycle_request_invalid"}, status=400)
    try:
        package = install_workspace_plugin(
            request.user, workspace_id, UploadedWorkspaceZip(request.FILES["file"])
        )
        from .workspaces import _workspace_plugin

        workspace = Workspace.objects.get(pk=workspace_id)
        return JsonResponse(
            {"plugin": _workspace_plugin(package, True, workspace=workspace)}
        )
    except PermissionError:
        return JsonResponse({"error": "workspace_not_found"}, status=404)
    except PluginInstallSourceError as error:
        return JsonResponse(
            {"error": error.code},
            status=413 if error.code == "plugin_archive_too_large" else 400,
        )
    except PluginLifecycleError as error:
        return JsonResponse({"error": error.code}, status=409)
    except (OSError, RuntimeError, ValueError):
        return JsonResponse({"error": "plugin_package_invalid"}, status=400)


@router.delete(
    "/workspaces/{workspace_id}/plugins/{plugin_name}/installation",
    auth=session_auth,
    response={204: None} | COMMON_ERROR_RESPONSES,
)
def uninstall_plugin(request, workspace_id: str, plugin_name: str):
    from app_core.workspace_plugins import remove_workspace_plugin
    from app_core.plugin_lifecycle import PluginLifecycleError

    try:
        remove_workspace_plugin(request.user, workspace_id, plugin_name)
        return HttpResponse(status=204)
    except PermissionError:
        return JsonResponse({"error": "workspace_not_found"}, status=404)
    except PluginLifecycleError as error:
        return JsonResponse({"error": error.code}, status=409)
