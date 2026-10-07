"""Immutable attachment grants belong to accepted inputs, not mutable drafts."""
from types import SimpleNamespace

from .assets import DeferredInputResolutionError, resolved_input_for_link
from .models import SessionAssetLink


MAX_INPUT_ATTACHMENTS = 50


def validate_attachment_refs(refs):
    if (not isinstance(refs, list) or len(refs) > MAX_INPUT_ATTACHMENTS
            or any(not isinstance(ref, str) or not ref or ref.strip() != ref for ref in refs)
            or refs != sorted(set(refs))):
        raise ValueError("agent_input_attachments_invalid")
    return refs


def capture_attachments(user, session, refs):
    validate_attachment_refs(refs)
    links = list(SessionAssetLink.objects.select_related("sourceObject__source", "userLibraryObject", "artifact")
        .filter(id__in=refs, session=session, workspace_id=session.workspace_id, attachedBy=user).order_by("id"))
    if [link.pk for link in links] != refs:
        raise DeferredInputResolutionError("asset_unavailable")
    scope = SimpleNamespace(user=user, user_id=user.pk, session_id=session.pk)
    captures = []
    for link in links:
        resolved_input_for_link(scope, link, f"/mnt/data/inputs/{link.pk}")
        captures.append({"schema": "runtime.declared_input.v1", "inputRef": link.pk,
            "displayName": link.capturedDisplayName, "contentType": link.capturedContentType,
            "inputIdentity": {"ownerKind": link.capturedOwnerKind, "ownerId": link.capturedOwnerId,
                "generation": link.capturedContentGeneration, "sha256": link.capturedSha256},
            "sizeBytes": link.capturedSizeBytes})
    return captures


def delivered_attachment(run, input_ref):
    return next((item for item in delivered_attachments(run) if item["inputRef"] == input_ref), None)


def delivered_attachments(run):
    from .models import AgentInputDelivery
    deliveries = AgentInputDelivery.objects.filter(queue__agent_run=run,
        queue__authorization_digest=run.authorization.digest, input__session_id=run.session_id,
        input__membership_ref=run.membership_ref).select_related("input")
    captures = {}
    for delivery in deliveries:
        for declared in delivery.input.attachments:
            current = captures.get(declared["inputRef"])
            if current is not None and current != declared:
                raise DeferredInputResolutionError("stale_generation")
            captures[declared["inputRef"]] = declared
    return [captures[ref] for ref in sorted(captures)]


def resolve_agent_attachment(user, agent_id, input_ref, input_id=None):
    from django.core.files.storage import default_storage
    from .agent_inputs import AgentInputError, owned_input_binding
    from .models import AgentInput, WorkspaceMembership
    binding = owned_input_binding(user, agent_id)
    capture = None
    if input_id is not None:
        fact = AgentInput.objects.filter(agent_id=agent_id, session_id=binding.session_id, input_id=input_id).first()
        if fact is None or not WorkspaceMembership.objects.filter(pk=fact.membership_ref,
                workspace_id=binding.agent.workspace_id, user=user, role__in=["owner", "admin", "member"]).exists():
            raise AgentInputError(404, "agent_input_attachment_rejected")
        capture = next((item for item in fact.attachments if item["inputRef"] == input_ref), None)
        if capture is None:
            raise AgentInputError(404, "agent_input_attachment_rejected")
    current = capture_attachments(user, binding.session, [input_ref])[0]
    if capture is not None and capture != current:
        raise DeferredInputResolutionError("stale_generation")
    link = SessionAssetLink.objects.select_related("sourceObject__source", "userLibraryObject", "artifact").get(pk=input_ref)
    resolved = resolved_input_for_link(SimpleNamespace(user=user, user_id=user.pk, session_id=binding.session_id),
        link, f"/mnt/data/inputs/{input_ref}")
    owner = link.sourceObject or link.userLibraryObject or link.artifact
    if not owner.storageKey or not default_storage.exists(owner.storageKey):
        raise DeferredInputResolutionError("asset_unavailable")
    return resolved, owner.storageKey
