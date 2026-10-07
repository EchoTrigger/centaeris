"""Per-request material authorization and current input binding.

The transport authenticates its caller before constructing MaterialAccessContext.
This context is not an authentication token and must never be taken from model
arguments. Authorize each request; do not cache MaterialAccess across tool calls.
Input resolution retains current permission, signature and source identity checks.
"""

from dataclasses import dataclass

from .assets import DeferredInputResolutionError
from .agent_input_attachments import delivered_attachments
from .deferred_input import DeferredInputBindingError, input_storage_batch
from .material_contract import BoundInput, KnowledgeError
from .material_identity import processing_spec_digest, representation_id
from .models import AgentRun, SessionAssetLink
from .runtime_contract import authorization_digest
from .workspace_access import agent_run_membership_is_current


@dataclass(frozen=True)
class MaterialAccessContext:
    agent_run_id: str
    authorization_digest: str
    processing_specification: dict
    spec_digest: str


@dataclass(frozen=True)
class MaterialAccess:
    agent_run: AgentRun
    processing_specification: dict
    spec_digest: str
    authorization_digest: str


def authorize_material_access(context: MaterialAccessContext) -> MaterialAccess:
    try:
        agent_run = AgentRun.objects.select_related("authorization", "user").get(id=context.agent_run_id)
    except (KeyError, AgentRun.DoesNotExist) as error:
        raise KnowledgeError("knowledge_agent_run_not_found", 404) from error
    if not agent_run_membership_is_current(agent_run):
        raise KnowledgeError("knowledge_authorization_mismatch", 403)
    authorization = agent_run.authorization
    if (
        authorization_digest(authorization.payload) != authorization.digest
        or context.authorization_digest != authorization.digest
    ):
        raise KnowledgeError("knowledge_authorization_mismatch", 403)
    specification = context.processing_specification
    spec_digest = processing_spec_digest(specification)
    if context.spec_digest != spec_digest:
        raise KnowledgeError("knowledge_specification_digest_mismatch")
    return MaterialAccess(agent_run, specification, spec_digest, authorization.digest)


def declared_inputs_for_run(agent_run):
    declared = {item["inputRef"]: item for item in agent_run.authorization.payload["assetRefs"]}
    for capture in delivered_attachments(agent_run):
        if capture["inputRef"] in declared and declared[capture["inputRef"]] != capture:
            raise KnowledgeError("knowledge_input_binding_invalid")
        declared[capture["inputRef"]] = capture
    return declared


def bind_inputs(agent_run, digest: str, requests: list, spec_digest: str) -> list[BoundInput]:
    if not isinstance(requests, list):
        raise KnowledgeError("knowledge_inputs_invalid", 400)
    input_refs = []
    for request in requests:
        if (
            not isinstance(request, dict)
            or set(request) != {"inputRef", "representationId"}
            or not isinstance(request["inputRef"], str)
            or not request["inputRef"].strip()
        ):
            raise KnowledgeError("knowledge_input_fields_invalid", 400)
        input_refs.append(request["inputRef"])
    if len(set(input_refs)) != len(input_refs):
        raise KnowledgeError("knowledge_inputs_invalid", 400)
    declared_by_ref = declared_inputs_for_run(agent_run)
    bound = []
    resolve_storage = input_storage_batch(agent_run, digest)
    for request in requests:
        declared = declared_by_ref.get(request["inputRef"])
        if declared is None:
            raise KnowledgeError("knowledge_input_not_authorized", 403)
        identity = declared["inputIdentity"]
        expected = representation_id(identity, spec_digest)
        if request["representationId"] != expected:
            raise KnowledgeError("knowledge_representation_binding_mismatch")
        try:
            resolved, storage_key = resolve_storage(request["inputRef"])
        except DeferredInputResolutionError as error:
            raise KnowledgeError(error.errorCode) from error
        except DeferredInputBindingError as error:
            raise KnowledgeError("knowledge_input_binding_invalid") from error
        owner = _input_owner(agent_run, request["inputRef"], resolved)
        bound.append(BoundInput(resolved, storage_key, owner, identity, expected))
    return bound


def _input_owner(agent_run, input_ref: str, resolved: dict):
    try:
        link = SessionAssetLink.objects.select_related(
            "sourceObject", "userLibraryObject", "artifact"
        ).get(id=input_ref, session=agent_run.session, workspace=agent_run.workspace)
    except SessionAssetLink.DoesNotExist as error:
        raise KnowledgeError("knowledge_input_not_authorized", 403) from error
    owner = link.sourceObject or link.userLibraryObject or link.artifact
    if owner is None or owner.id != resolved["objectRef"]:
        raise KnowledgeError("knowledge_input_binding_invalid")
    return owner
