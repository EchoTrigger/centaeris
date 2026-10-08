"""User-owned delegated app access to existing hosted resources."""
import hashlib
import re

from django.contrib.auth import get_user_model
from django.db.models import Q
from django.utils import timezone

from .app_delegation_contract import AUDIENCE, ISSUER, NATIVE_SCOPES, SCOPES
from .agent_definitions import available_agent_definitions
from .models import Agent, AgentRun, Artifact, BusinessAgentBranch, BusinessApplication, HostedOperationReceipt, Session, SessionAssetLink, SessionCitationProjection, UserAppDelegation
from .workspace_access import workspace_membership_for


class DelegationRejected(Exception):
    def __init__(self, code, status=403):
        self.code, self.status = code, status
        super().__init__(code)


def token_digest(token):
    return "sha256:" + hashlib.sha256(token.encode("ascii")).hexdigest()


def require_current_delegation(delegation_id, scope=None, *, lock=False, expected_credential_version=None):
    if scope is not None and scope not in SCOPES:
        raise ValueError("unknown delegation scope")
    query = UserAppDelegation.objects.select_related("user", "app", "workspace", "definition", "agent")
    if lock:
        app_id = UserAppDelegation.objects.filter(id=delegation_id).values_list("app_id", flat=True).first()
        BusinessApplication.objects.select_for_update().filter(id=app_id).first()
        query = query.select_for_update(of=("self",))
    grant = query.filter(id=delegation_id).first()
    if expected_credential_version is not None and (grant is None
            or type(expected_credential_version) is not int or expected_credential_version <= 0
            or grant.credential_version != expected_credential_version):
        raise DelegationRejected("delegation_invalid", 401)
    if (grant is None or not grant.user.is_active or grant.app.status != "active"
            or grant.revoked_at is not None or (grant.expires_at is not None and grant.expires_at <= timezone.now())
            or grant.issuer != ISSUER or grant.audience != AUDIENCE
            or (grant.definition_id is None) == (grant.agent_id is None)):
        raise DelegationRejected("delegation_not_available")
    membership = workspace_membership_for(grant.user, grant.workspace_id)
    if membership is None or membership.id != grant.membership_ref:
        raise DelegationRejected("delegation_not_available")
    if grant.agent_id:
        if (grant.agent.workspace_id != grant.workspace_id or grant.agent.owner_id != grant.user_id
                or grant.agent.definition_id is not None or grant.agent.status != "active"
                or BusinessAgentBranch.objects.filter(agent_id=grant.agent_id).exists()
                or not set(grant.scopes) <= NATIVE_SCOPES):
            raise DelegationRejected("delegation_not_available")
    elif (grant.definition.workspace_id != grant.workspace_id
            or not available_agent_definitions(membership).filter(id=grant.definition_id).exists()):
        raise DelegationRejected("delegation_not_available")
    if scope is not None and scope not in grant.scopes:
        raise DelegationRejected("delegation_scope_forbidden")
    return grant


def authenticate_delegation(token, scope):
    if not isinstance(token, str) or re.fullmatch(r"cwa_[A-Za-z0-9_-]{43}", token) is None:
        raise DelegationRejected("delegation_invalid", 401)
    credential = UserAppDelegation.objects.filter(token_digest=token_digest(token)).values_list("id", "credential_version").first()
    if credential is None:
        raise DelegationRejected("delegation_invalid", 401)
    return require_current_delegation(credential[0], scope, expected_credential_version=credential[1])


def require_delegated_agent(grant, agent_id, *, require_active=True, business_branch=None):
    if grant.agent_id or business_branch is not None or getattr(grant, "business_branch", None) is not None:
        from .business_agent_branches import require_business_branch
        branch = business_branch if business_branch is not None else getattr(grant, "business_branch", None)
        if branch is None:
            raise DelegationRejected("business_branch_required", 400)
        branch = require_business_branch(grant, branch.pk, require_active=require_active)
        if branch.agent_id != agent_id:
            raise DelegationRejected("business_branch_not_found", 404)
        return branch.agent
    query = Agent.objects.filter(id=agent_id, workspace_id=grant.workspace_id, owner_id=grant.user_id,
                                 definition_id=grant.definition_id, is_business_instance=False)
    if require_active:
        query = query.filter(status="active")
    agent = query.first()
    if agent is None:
        raise DelegationRejected("agent_not_found", 404)
    return agent


def require_delegated_session(grant, session_id, *, require_active=True, business_branch=None):
    query = Session.objects.filter(id=session_id, workspace_id=grant.workspace_id, owner_id=grant.user_id,
        agent__owner_id=grant.user_id, agent__definition_id=grant.definition_id)
    if grant.agent_id or business_branch is not None or getattr(grant, "business_branch", None) is not None:
        from .business_agent_branches import require_business_branch
        branch = business_branch if business_branch is not None else getattr(grant, "business_branch", None)
        if branch is None:
            raise DelegationRejected("business_branch_required", 400)
        branch = require_business_branch(grant, branch.pk, require_active=require_active)
        query = query.filter(agent_id=branch.agent_id).filter(
            Q(pk=branch.session_id) | Q(work_binding__coordination_session_id=branch.session_id))
    else:
        query = query.filter(agent__is_business_instance=False)
    if require_active:
        query = query.filter(status="active", agent__status="active")
    session = query.first()
    if session is None:
        raise DelegationRejected("business_branch_not_found" if grant.agent_id else "session_not_found", 404)
    return session


def require_request_delegation(request, scope, *, workspace_id=None, agent_id=None, session_id=None,
                               additional_scopes=(), lock=False, require_active=True):
    grant = getattr(request, "app_delegation", None)
    if grant is None:
        return None
    version = getattr(request, "app_delegation_credential_version", None)
    if type(version) is not int or version <= 0:
        raise DelegationRejected("delegation_invalid", 401)
    grant = require_current_delegation(grant.id, scope, lock=lock, expected_credential_version=version)
    if grant.agent_id or getattr(request, "business_branch_id", None) is not None:
        from .business_agent_branches import require_business_branch
        branch_id = getattr(request, "business_branch_id", None)
        if branch_id is None:
            raise DelegationRejected("business_branch_required", 400)
        branch = require_business_branch(grant, branch_id, lock=lock, require_active=require_active)
        request.business_branch = branch
        grant.business_branch = branch
    for required_scope in additional_scopes:
        if required_scope not in SCOPES:
            raise ValueError("unknown delegation scope")
        if required_scope not in grant.scopes:
            raise DelegationRejected("delegation_scope_forbidden")
    if workspace_id is not None and workspace_id != grant.workspace_id:
        raise DelegationRejected("workspace_not_found", 404)
    if agent_id is not None:
        require_delegated_agent(grant, agent_id, require_active=require_active)
    if session_id is not None and session_id != "new":
        require_delegated_session(grant, session_id, require_active=require_active)
    request.app_delegation = grant
    return grant


def authorize_delegated_request(request, grant):
    values = request.resolver_match.kwargs
    if "workspace_id" in values and values["workspace_id"] != grant.workspace_id:
        raise DelegationRejected("workspace_not_found", 404)
    if "definition_id" in values and values["definition_id"] != grant.definition_id:
        raise DelegationRejected("agent_definition_not_found", 404)
    if "agent_id" in values:
        require_delegated_agent(grant, values["agent_id"])
    if "session_id" in values:
        if grant.agent_id and request.method not in {"GET", "HEAD"}:
            # Native business inputs use the owner-run coordinator intake.
            # They never create a generic Session Run with one application's origin.
            raise DelegationRejected("delegation_scope_forbidden")
        if values["session_id"] == "new":
            if "sessions:create" not in grant.scopes:
                raise DelegationRejected("delegation_scope_forbidden")
        else:
            # Message retries must reach their existing receipt before checking
            # lifecycle state. Ownership and assistant binding still apply.
            message_submission = request.method == "POST" and request.resolver_match.url_name == "create_session_message"
            require_delegated_session(grant, values["session_id"], require_active=not message_submission)
        if "agent_run_id" in values and not AgentRun.objects.filter(id=values["agent_run_id"],
            session_id=values["session_id"], user_id=grant.user_id, workspace_id=grant.workspace_id).exists():
            raise DelegationRejected("agent_run_not_found", 404)
    if "operation_id" in values:
        receipt = HostedOperationReceipt.objects.filter(user_id=grant.user_id, workspace_id=grant.workspace_id,
            command=values["command"], operationId=values["operation_id"]).first()
        if receipt is None:
            raise DelegationRejected("operation_not_found", 404)
        require_delegated_session(grant, receipt.sessionId, require_active=False)
    if "artifact_id" in values:
        session_id = Artifact.objects.filter(id=values["artifact_id"], createdBy_id=grant.user_id).values_list("session_id", flat=True).first()
        if session_id is None:
            raise DelegationRejected("artifact_not_found", 404)
        require_delegated_session(grant, session_id)
    if "citation_id" in values:
        session_id = SessionCitationProjection.objects.filter(citationId=values["citation_id"],
            agent_run__user_id=grant.user_id).values_list("session_id", flat=True).first()
        if session_id is None:
            raise DelegationRejected("citation_not_found", 404)
        require_delegated_session(grant, session_id)
    for path_field, object_field, owner_kind in [("library_object_id", "userLibraryObject_id", "userLibraryObject"),
                                                ("source_object_id", "sourceObject_id", "sourceObject")]:
        if path_field not in values:
            continue
        scope = dict(session__workspace_id=grant.workspace_id, session__owner_id=grant.user_id,
                     session__status="active", session__agent__status="active", session__agent__definition_id=grant.definition_id)
        if not grant.agent_id:
            scope["session__agent__is_business_instance"] = False
        linked = SessionAssetLink.objects.filter(**scope, **{object_field: values[path_field]})
        cited = SessionCitationProjection.objects.filter(**scope, agent_run__user_id=grant.user_id,
            ownerKind=owner_kind, ownerRef=values[path_field])
        if grant.agent_id:
            branch = request.business_branch
            native_sessions = (Q(session_id=branch.session_id)
                | Q(session__work_binding__coordination_session_id=branch.session_id))
            linked = linked.filter(session__agent_id=branch.agent_id).filter(native_sessions)
            cited = cited.filter(session__agent_id=branch.agent_id).filter(native_sessions)
        if not linked.exists() and not cited.exists():
            raise DelegationRejected("object_not_found", 404)


def session_authority_is_current(user_id, session_id, delegation_id=None, scope="events:read", *, credential_version=None,
                                 business_branch_id=None):
    user = get_user_model().objects.filter(id=user_id, is_active=True).first()
    session = Session.objects.select_related("agent").filter(id=session_id, owner_id=user_id,
        status="active", agent__owner_id=user_id, agent__status="active").first()
    if user is None or session is None:
        return False
    membership = workspace_membership_for(user, session.workspace_id)
    if membership is None:
        return False
    if session.agent.definition_id and not available_agent_definitions(membership).filter(id=session.agent.definition_id).exists():
        return False
    if delegation_id is not None:
        try:
            grant = require_current_delegation(delegation_id, scope, expected_credential_version=credential_version)
            if grant.user_id != user_id:
                return False
            branch = None
            if grant.agent_id:
                from .business_agent_branches import require_business_branch
                if business_branch_id is None:
                    return False
                branch = require_business_branch(grant, business_branch_id)
            require_delegated_session(grant, session_id, business_branch=branch)
        except DelegationRejected:
            return False
    return True


def require_run_delegation(run, *, lock=False):
    if run.acting_app_id is None and run.app_delegation_id is None:
        return
    if run.acting_app_id is None or run.app_delegation_id is None:
        raise DelegationRejected("delegation_not_available")
    grant = require_current_delegation(run.app_delegation_id, "messages:submit", lock=lock)
    if (grant.agent_id is not None or grant.app_id != run.acting_app_id
            or grant.user_id != run.user_id or grant.workspace_id != run.workspace_id):
        raise DelegationRejected("delegation_not_available")
    require_delegated_session(grant, run.session_id)
