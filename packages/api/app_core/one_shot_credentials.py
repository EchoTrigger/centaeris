"""Credential scope for one-shot execution and its mutable authority checks."""

import hashlib
import secrets

from django.db import transaction
from django.db.models import Q

from .models import OneShotCredential, OneShotOperation, SourceObject, WorkspaceMembership
from .plugin_catalog import (
    activation_digest,
    plugin_activation_for_workspace,
    selected_plugin_activation,
    validate_plugin_names,
)
from .workspace_access import WORKSPACE_ADMIN_ROLES, locked_workspace_membership_for
from .workspace_resources import models_for_workspace


class CredentialRejected(ValueError):
    def __init__(self, code, status=403):
        self.code = code
        self.status = status
        super().__init__(code)


def _enabled_models(workspace_id):
    return models_for_workspace(workspace_id).filter(
        Q(provider__isnull=True)
        | Q(provider__enabled=True, provider__archivedAt__isnull=True),
        enabled=True,
        isCurrent=True,
    )


def authority_is_current(credential) -> bool:
    """Re-read credential, sponsor, workspace and model at an authority boundary."""
    return OneShotCredential.objects.filter(
        pk=credential.pk,
        revokedAt__isnull=True,
        workspace__status="active",
        user__is_active=True,
        membership_ref__in=WorkspaceMembership.objects.filter(
            pk=credential.membership_ref,
            workspace_id=credential.workspace_id,
            user_id=credential.user_id,
            role__in=WORKSPACE_ADMIN_ROLES,
        ).values("pk"),
        modelConfig_id__in=_enabled_models(credential.workspace_id).values("pk"),
    ).exists()


def credential_for_run(run):
    if run.session.purpose != "oneShot":
        return None
    operation = OneShotOperation.objects.select_related("credential").filter(
        agentRunId=run.pk,
        sessionId=run.session_id,
        credential__workspace_id=run.workspace_id,
        credential__user_id=run.user_id,
        credential__membership_ref=run.membership_ref,
        credential__modelConfig_id=run.modelConfig_id,
    ).first()
    return operation.credential if operation is not None else None


def source_object_allowed(credential, item) -> bool:
    from .assets import source_object_is_granted

    return (
        item.pk in credential.sourceObjectRefs
        and item.workspace_id == credential.workspace_id
        and item.objectType == "file"
        and item.status == "ready"
        and source_object_is_granted(credential.user, item)
    )


def plugin_activation_for_run(run):
    if run.session.purpose != "oneShot":
        return plugin_activation_for_workspace(run.workspace)
    credential = credential_for_run(run)
    if credential is None:
        raise CredentialRejected("api_credential_binding_invalid")
    if not credential.pluginNames:
        return selected_plugin_activation(run.workspace, [])
    available = plugin_activation_for_workspace(run.workspace)
    packages = [package for package in available["packages"] if package["name"] in credential.pluginNames]
    return {"schema": available["schema"], "digest": activation_digest(packages), "packages": packages}


def create_credential(
    user,
    workspace_id,
    model_config_ref,
    *,
    name,
    plugin_names=None,
    source_object_refs=None,
):
    plugin_names = sorted(set(plugin_names or []))
    source_object_refs = sorted(set(source_object_refs or []))
    validate_plugin_names(plugin_names)
    if not isinstance(name, str) or not name.strip() or len(name.strip()) > 120:
        raise CredentialRejected("api_credential_name_invalid", 400)
    if any(not isinstance(ref, str) or not ref for ref in source_object_refs):
        raise CredentialRejected("api_credential_sources_invalid", 400)
    with transaction.atomic():
        membership = locked_workspace_membership_for(
            user, workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES,
        )
        if membership is None:
            raise CredentialRejected("api_credential_forbidden")
        model = _enabled_models(workspace_id).filter(pk=model_config_ref).first()
        if model is None:
            raise CredentialRejected("model_not_found", 404)
        try:
            selected_plugin_activation(membership.workspace, plugin_names)
        except ValueError as error:
            raise CredentialRejected("api_credential_plugin_unavailable", 400) from error
        credential = OneShotCredential(
            user=user,
            workspace=membership.workspace,
            membership_ref=membership.pk,
            name=name.strip(),
            modelConfig=model,
            pluginNames=plugin_names,
            sourceObjectRefs=source_object_refs,
        )
        objects = SourceObject.objects.select_related("source").filter(pk__in=source_object_refs)
        if len(objects) != len(source_object_refs) or any(
            not source_object_allowed(credential, item) for item in objects
        ):
            raise CredentialRejected("api_credential_source_not_found", 404)
        token = f"{credential.pk}.{secrets.token_urlsafe(32)}"
        credential.secretDigest = hashlib.sha256(token.encode()).hexdigest()
        credential.save()
    return credential, token


def authenticate_credential(token, *, require_current=True):
    if not isinstance(token, str) or len(token) > 256:
        raise CredentialRejected("api_credential_invalid", 401)
    credential_id, separator, secret = token.partition(".")
    if not separator or not secret:
        raise CredentialRejected("api_credential_invalid", 401)
    credential = OneShotCredential.objects.select_related("workspace", "user", "modelConfig").filter(
        pk=credential_id,
    ).first()
    digest = hashlib.sha256(token.encode()).hexdigest()
    if credential is None or not secrets.compare_digest(credential.secretDigest, digest):
        raise CredentialRejected("api_credential_invalid", 401)
    if require_current and not authority_is_current(credential):
        raise CredentialRejected("api_credential_revoked")
    return credential
