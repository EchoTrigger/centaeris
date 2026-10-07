"""Validate one native private dispatch call without accepting hosted work."""
from django.conf import settings
from django.core.exceptions import ObjectDoesNotExist
from django.db import transaction

from .agent_messages import _digest
from .app_delegations import session_authority_is_current
from .assets import DeferredInputResolutionError
from .business_agent_scope import run_session_ref_is_current
from .deferred_input import DeferredInputBindingError, input_storage_batch
from .models import AgentCoordinationSession, AgentRun, SessionAssetLink
from .runtime_contract import (agent_run_binding_matches, authorization_digest, require_opaque_ref,
                               _verify_authorization_digest_signature)
from .workspace_access import agent_run_membership_is_current


class AgentWorkRejected(ValueError):
    pass


def validate_work_args(args):
    if not isinstance(args, dict) or set(args) != {"objective", "session_refs", "file_refs"}:
        raise ValueError("agent_work_input_invalid")
    objective = args["objective"]
    if not isinstance(objective, str) or not objective.strip():
        raise ValueError("agent_work_objective_invalid")
    try:
        if len(objective.encode("utf-8")) > 65536:
            raise ValueError("agent_work_objective_invalid")
    except UnicodeError as error:
        raise ValueError("agent_work_objective_invalid") from error
    for field in ("session_refs", "file_refs"):
        refs = args[field]
        if not isinstance(refs, list) or len(refs) > 8:
            raise ValueError("agent_work_refs_invalid")
        for ref in refs:
            require_opaque_ref(field, ref)
    return args


@transaction.atomic
def validate_agent_work(body):
    args = validate_work_args({"objective": body["objective"], "session_refs": body["sessionRefs"],
                              "file_refs": body["fileRefs"]})
    try:
        run = AgentRun.objects.select_related("session__agent", "authorization", "user").get(id=body["agentRunId"])
        agent = run.session.agent
        authorization = run.authorization
        digest = authorization_digest(authorization.payload)
        _verify_authorization_digest_signature(digest, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY,
                                               authorization.signature)
        if (digest != authorization.digest or digest != body["authorizationDigest"]
                or not agent_run_binding_matches(authorization.payload, run)
                or not agent_run_membership_is_current(run) or not run.user.is_active
                or run.status not in {"queued", "running"}
                or agent.definition_id or run.definition_version_id or run.acting_app_id or run.app_delegation_id
                or agent.owner_id != run.user_id or agent.workspace_id != run.workspace_id
                or run.session.owner_id != run.user_id or run.session.workspace_id != run.workspace_id
                or body["coordinationSessionId"] != run.session_id
                or not AgentCoordinationSession.objects.filter(agent=agent, session_id=run.session_id).exists()
                or not session_authority_is_current(run.user_id, run.session_id, scope="messages:submit")):
            raise AgentWorkRejected("agent_work_authority_rejected")
        for ref in args["session_refs"]:
            if not run_session_ref_is_current(run, ref):
                raise AgentWorkRejected("agent_work_session_ref_rejected")
        if SessionAssetLink.objects.filter(session=run.session, id__in=args["file_refs"], artifact__isnull=False).exists():
            raise AgentWorkRejected("agent_work_file_scope_not_supported")
        resolve = input_storage_batch(run, digest)
        for ref in args["file_refs"]:
            resolve(ref)
        return {"schema": "workspace.agent_work.validated.v1", "agentId": agent.id,
                "sessionId": run.session_id, "agentRunId": run.id, "toolCallId": body["toolCallId"],
                "authorizationDigest": digest, "inputDigest": _digest("workspace.agent_work.input.v1", args)}
    except (ObjectDoesNotExist, DeferredInputResolutionError, DeferredInputBindingError, ValueError) as error:
        raise AgentWorkRejected("agent_work_authority_rejected") from error
