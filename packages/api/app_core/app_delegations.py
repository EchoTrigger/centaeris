"""Retired Agent-target grants never authorize Session access."""
import hashlib
from django.contrib.auth import get_user_model
from .models import Session
from .workspace_access import workspace_membership_for

class DelegationRejected(Exception):
    def __init__(self, code, status=403):
        self.code, self.status = code, status
        super().__init__(code)

def token_digest(token):
    return "sha256:" + hashlib.sha256(token.encode("ascii")).hexdigest()

def require_current_delegation(*args, **kwargs):
    raise DelegationRejected("delegation_not_available")

def authenticate_delegation(*args, **kwargs):
    raise DelegationRejected("delegation_not_available", 401)

def authorize_delegated_request(*args, **kwargs):
    raise DelegationRejected("delegation_not_available")

def require_request_delegation(request, scope, **kwargs):
    if getattr(request, "app_delegation", None) is not None:
        raise DelegationRejected("delegation_not_available")
    return None

def require_delegated_session(*args, **kwargs):
    raise DelegationRejected("delegation_not_available")

def require_run_delegation(run, **kwargs):
    if run.acting_app_id is not None or run.app_delegation_id is not None:
        raise DelegationRejected("delegation_not_available")

def session_authority_is_current(user_id, session_id, delegation_id=None, scope="events:read", **kwargs):
    if delegation_id is not None:
        return False
    user = get_user_model().objects.filter(id=user_id, is_active=True).first()
    session = Session.objects.filter(id=session_id, owner_id=user_id, status="active").first()
    return bool(user and session and workspace_membership_for(user, session.workspace_id))
