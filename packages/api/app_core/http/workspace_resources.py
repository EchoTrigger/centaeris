"""Tenant administration of local resources; platform resources stay read-only."""

from types import SimpleNamespace
from django.db import IntegrityError, transaction
from django.db.models import Q
from django.http import JsonResponse, HttpResponse
from ninja import Router, Status
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
from .assistant_connectors import ConnectorApprovalRequest
from .serialization import serialize_model_provider, serialize_admin_model

router = Router(tags=["workspace resources"], by_alias=True)


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
    "/workspaces/{workspace_id}/model-providers", auth=session_auth, response=None
)
def providers(request, workspace_id: str):
    with transaction.atomic():
        try:
            workspace_resource_admin(request.user, workspace_id)
        except PermissionError:
            return JsonResponse({"error": "workspace_not_found"}, status=404)
        items = ModelProvider.objects.filter(
            Q(workspace_id=workspace_id) | Q(workspace__isnull=True),
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


@router.get("/workspaces/{workspace_id}/models", auth=session_auth, response=None)
def models(request, workspace_id: str):
    with transaction.atomic():
        try:
            workspace_resource_admin(request.user, workspace_id)
        except PermissionError:
            return JsonResponse({"error": "workspace_not_found"}, status=404)
        items = (
            ModelConfig.objects.select_related("provider")
            .filter(
                Q(provider__workspace_id=workspace_id)
                | Q(provider__workspace__isnull=True),
                isCurrent=True,
                provider__archivedAt__isnull=True,
            )
            .order_by("displayName", "id")
        )
        return JsonResponse(
            {
                "models": [
                    serialize_admin_model(x)
                    | resource_scope(x.provider.workspace_id if x.provider_id else None)
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
    response=None,
)
def credentials(request, workspace_id: str):
    with transaction.atomic():
        try:
            workspace_resource_admin(request.user, workspace_id)
        except PermissionError:
            return JsonResponse({"error": "workspace_not_found"}, status=404)
        items = McpBearerCredential.objects.filter(
            Q(workspace_id=workspace_id) | Q(workspace__isnull=True)
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
    response=None,
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
    response=None,
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
    response=None,
)
def remove_credential(request, workspace_id: str, credential_id: str):
    try:
        with transaction.atomic():
            workspace_resource_admin(request.user, workspace_id)
            credential = McpBearerCredential.objects.select_for_update().get(
                pk=credential_id, workspace_id=workspace_id
            )
            if credential.assistant_approvals.exists():
                return JsonResponse(
                    {"error": "mcp_bearer_credential_in_use"}, status=409
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
    response=None,
)
def provider_templates(request, workspace_id: str):
    return model_action(request, workspace_id, mm.list_model_provider_templates)


@router.post(
    "/workspaces/{workspace_id}/model-provider-templates/{template_id}/instantiate",
    auth=session_auth,
    response=None,
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
    "/workspaces/{workspace_id}/model-providers", auth=session_auth, response=None
)
def create_provider(request, workspace_id: str, payload: mm.CreateModelProviderRequest):
    return model_action(
        request, workspace_id, mm.create_model_provider, payload=payload
    )


@router.patch(
    "/workspaces/{workspace_id}/model-providers/{provider_id}",
    auth=session_auth,
    response=None,
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
    response=None,
)
def delete_provider(request, workspace_id: str, provider_id: str):
    return model_action(
        request, workspace_id, mm.delete_model_provider, provider_id=provider_id
    )


@router.post(
    "/workspaces/{workspace_id}/model-providers/{provider_id}/credential/rotate",
    auth=session_auth,
    response=None,
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


@router.post("/workspaces/{workspace_id}/models", auth=session_auth, response=None)
def create_model(request, workspace_id: str, payload: mm.CreateModelRequest):
    return model_action(request, workspace_id, mm.create_model, payload=payload)


@router.patch(
    "/workspaces/{workspace_id}/models/{model_id}", auth=session_auth, response=None
)
def update_model(
    request, workspace_id: str, model_id: str, payload: mm.UpdateModelRequest
):
    return model_action(
        request, workspace_id, mm.update_model, model_id=model_id, payload=payload
    )


@router.delete(
    "/workspaces/{workspace_id}/models/{model_id}", auth=session_auth, response=None
)
def delete_model(request, workspace_id: str, model_id: str):
    return model_action(request, workspace_id, mm.delete_model, model_id=model_id)


@router.post(
    "/workspaces/{workspace_id}/models/{model_id}/test",
    auth=session_auth,
    response=None,
)
def test_model(
    request, workspace_id: str, model_id: str, payload: mm.EmptyRequest | None = None
):
    return model_action(
        request, workspace_id, mm.test_model, model_id=model_id, payload=payload
    )


@router.post(
    "/workspaces/{workspace_id}/plugins/upload", auth=session_auth, response=None
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
    response=None,
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


@router.post(
    "/workspaces/{workspace_id}/mcp-bearer-credentials/{credential_id}/assistant-approvals",
    auth=session_auth,
    response=None,
)
def approve_local_assistant_credential(
    request, workspace_id: str, credential_id: str, payload: ConnectorApprovalRequest
):
    from app_core.models import AgentDefinition, AgentConnectorCredentialApproval
    from app_core.assistant_connectors import connector_resource, ConnectorRejected
    from app_core.plugin_catalog import require_current_plugin_activation
    from .assistant_connectors import _approval_response

    try:
        with transaction.atomic():
            workspace = workspace_resource_admin(request.user, workspace_id)
            if payload.workspace_id != workspace.id:
                return JsonResponse(
                    {"error": "mcp_credential_approval_scope_invalid"}, status=400
                )
            definition = (
                AgentDefinition.objects.select_for_update(of=("self",))
                .filter(id=payload.definition_id, workspace=workspace)
                .select_related("published_version")
                .first()
            )
            credential = (
                McpBearerCredential.objects.select_for_update()
                .filter(id=credential_id, workspace=workspace)
                .first()
            )
            if definition is None or credential is None:
                return JsonResponse(
                    {"error": "mcp_credential_approval_scope_not_found"}, status=404
                )
            if definition.published_version is None:
                return JsonResponse(
                    {"error": "agent_definition_not_published"}, status=409
                )
            require_current_plugin_activation(
                workspace, definition.published_version.plugin_activation
            )
            resource = connector_resource(
                definition.published_version.plugin_activation,
                credential.plugin_name,
                payload.server_id,
            )
            if resource["credentialRef"] != credential.credential_ref:
                return JsonResponse(
                    {"error": "mcp_credential_approval_scope_invalid"}, status=400
                )
            approval = AgentConnectorCredentialApproval.objects.create(
                workspace=workspace,
                definition=definition,
                plugin_name=credential.plugin_name,
                server_id=payload.server_id,
                resource_path=resource["resourcePath"],
                resource_digest=resource["resourceDigest"],
                credential=credential,
                credential_version=credential.version,
                approved_by=request.user,
            )
            return JsonResponse({"approval": _approval_response(approval)}, status=201)
    except PermissionError:
        return JsonResponse({"error": "workspace_not_found"}, status=404)
    except (ConnectorRejected, ValueError, KeyError, OSError):
        return JsonResponse(
            {"error": "mcp_credential_approval_scope_invalid"}, status=400
        )


@router.delete(
    "/workspaces/{workspace_id}/mcp-assistant-credential-approvals/{approval_id}",
    auth=session_auth,
    response=None,
)
def revoke_local_assistant_credential(request, workspace_id: str, approval_id: str):
    from django.utils import timezone
    from app_core.models import AgentConnectorCredentialApproval

    try:
        with transaction.atomic():
            workspace_resource_admin(request.user, workspace_id)
            approval = (
                AgentConnectorCredentialApproval.objects.select_for_update()
                .filter(
                    pk=approval_id,
                    workspace_id=workspace_id,
                    credential__workspace_id=workspace_id,
                )
                .first()
            )
            if approval is None:
                return JsonResponse(
                    {"error": "mcp_credential_approval_not_found"}, status=404
                )
            if approval.revoked_at is None:
                approval.revoked_at = timezone.now()
                approval.save(update_fields=["revoked_at"])
            return HttpResponse(status=204)
    except PermissionError:
        return JsonResponse({"error": "workspace_not_found"}, status=404)


@router.get("/admin/workspace-resources", auth=session_auth, response=None)
def operator_workspace_resources(request):
    # This is an explicit operator metadata endpoint, not workspace membership.
    if not request.user.is_superuser:
        return JsonResponse({"error": "resource_not_found"}, status=404)
    return JsonResponse(
        {
            "workspaces": list(
                Workspace.objects.order_by("name", "id").values("id", "name", "status")
            )
        }
    )
