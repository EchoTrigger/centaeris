import secrets

from django.conf import settings
from ninja.utils import check_csrf
from ninja.security import APIKeyHeader, HttpBearer, SessionAuth

from app_core.app_delegations import DelegationRejected, authenticate_delegation, authorize_delegated_request
from app_core.app_delegation_contract import SCOPES
from app_core.business_agent_branches import bind_request_business_branch


class PublicAuthenticationRequired(RuntimeError):
    pass


class InternalAuthenticationRequired(RuntimeError):
    pass


class PublicCsrfRejected(RuntimeError):
    pass


class SuperuserRequired(RuntimeError):
    pass


class ProductSessionAuth(SessionAuth):
    def __init__(self):
        # Authentication must win over CSRF for anonymous requests. Ninja's
        # SessionAuth checks CSRF before authenticate() by default, so keep the
        # check here where the authenticated principal is already known.
        super().__init__(csrf=False)

    def authenticate(self, request, key):
        if "Authorization" in request.headers:
            if settings.SESSION_COOKIE_NAME in request.COOKIES or request.user.is_authenticated:
                raise DelegationRejected("authentication_mixed", 400)
            raise PublicAuthenticationRequired("authentication_required")
        if request.user.is_authenticated:
            if request.method not in {"GET", "HEAD", "OPTIONS", "TRACE"}:
                if request.content_type == "application/json":
                    _ = request.body
                if check_csrf(request) is not None:
                    raise PublicCsrfRejected("csrf_failed")
            return request.user
        raise PublicAuthenticationRequired("authentication_required")


class ProductSuperuserAuth(ProductSessionAuth):
    def authenticate(self, request, key):
        user = super().authenticate(request, key)
        if not user.is_superuser:
            raise SuperuserRequired("superuser_required")
        return user


class ProductUsageAuth(HttpBearer):
    def __init__(self, scope):
        super().__init__()
        if scope not in SCOPES:
            raise ValueError("unknown delegation scope")
        self.scope = scope

    def __call__(self, request):
        request.app_delegation = None
        request.app_delegation_credential_version = None
        request.business_branch = None
        request.business_branch_id = None
        if "Authorization" not in request.headers:
            return session_auth(request)
        if settings.SESSION_COOKIE_NAME in request.COOKIES or request.user.is_authenticated:
            raise DelegationRejected("authentication_mixed", 400)
        parts = request.headers["Authorization"].split(" ")
        if len(parts) != 2 or parts[0].lower() != "bearer":
            raise DelegationRejected("delegation_invalid", 401)
        return self.authenticate(request, parts[1])

    def authenticate(self, request, token):
        grant = authenticate_delegation(token, self.scope)
        bind_request_business_branch(request, grant)
        authorize_delegated_request(request, grant)
        request.user = grant.user
        request.app_delegation = grant
        request.app_delegation_credential_version = grant.credential_version
        return grant.user


def usage_auth(scope):
    return ProductUsageAuth(scope)


class InternalTokenAuth(APIKeyHeader):
    param_name = "X-Internal-Token"

    def authenticate(self, request, key):
        if isinstance(key, str) and secrets.compare_digest(key, settings.INTERNAL_API_TOKEN):
            return key
        raise InternalAuthenticationRequired("unauthorized")


def require_public_csrf(request):
    """Run CSRF before Ninja parses an unauthenticated mutation body."""
    if check_csrf(request) is not None:
        raise PublicCsrfRejected("csrf_failed")
    return True


session_auth = ProductSessionAuth()
superuser_auth = ProductSuperuserAuth()
internal_token_auth = InternalTokenAuth()
