"""OAuth transport is Authlib-owned; local account association remains explicit."""
from datetime import timedelta
import hashlib
import secrets
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit

from authlib.integrations.django_client import OAuth
from django.conf import settings
from django.contrib.auth import get_user_model, login, logout
from django.db import IntegrityError, transaction
from django.http import HttpResponseRedirect
from django.utils import timezone
from django.utils.crypto import constant_time_compare, salted_hmac

from .models import ExternalIdentity, SsoAuthorizationAttempt
from .sso_config import validate_providers


AUTHORIZATION_LIFETIME = timedelta(minutes=10)


class SsoRejected(Exception):
    def __init__(self, code, status=400):
        self.code = code
        self.status = status


def providers():
    return validate_providers(getattr(settings, "SSO_PROVIDERS", ()),
        allow_insecure_http=settings.DEBUG and getattr(settings, "SSO_ALLOW_INSECURE_HTTP", False))


def provider_for(provider_id):
    provider = next((p for p in providers() if p.provider_id == provider_id), None)
    if provider is None:
        raise SsoRejected("sso_provider_unavailable", 404)
    return provider


def oauth_client(provider):
    oauth = OAuth()
    return oauth.register(provider.provider_id, client_id=provider.client_id,
        client_secret=provider.client_secret.get_secret_value(), authorize_url=provider.authorize_url,
        access_token_url=provider.token_url, client_kwargs={"scope": provider.scope,
            "code_challenge_method": "S256", "token_endpoint_auth_method": provider.token_endpoint_auth_method,
            "timeout": 10})


def safe_return_to(value):
    if (not value.startswith("/") or value.startswith("//") or "\\" in value
            or any(ord(c) < 32 for c in value)):
        raise SsoRejected("sso_return_to_invalid")
    return value


def session_digest(request, *, create=True):
    if not request.session.session_key and create:
        request.session.create()
    if not request.session.session_key:
        return ""
    return salted_hmac("sso.browser-session", request.session.session_key, algorithm="sha256").hexdigest()


def begin_authorization(request, provider, *, purpose, return_to, current_password=""):
    safe_return_to(return_to)
    actor = None
    if purpose == "link":
        actor = get_user_model().objects.get(pk=request.user.pk, is_active=True)
        if not actor.check_password(current_password):
            raise SsoRejected("account_current_password_invalid", 403)
    elif request.user.is_authenticated:
        raise SsoRejected("sso_already_authenticated", 409)
    now = timezone.now()
    SsoAuthorizationAttempt.objects.filter(expires_at__lt=now).delete()
    binding = session_digest(request)
    if SsoAuthorizationAttempt.objects.filter(session_digest=binding, consumed_at__isnull=True).count() >= 5:
        raise SsoRejected("sso_too_many_attempts", 429)
    state = secrets.token_urlsafe(32)
    client = oauth_client(provider)
    response = client.authorize_redirect(request,
        settings.WEB_ORIGIN + f"/api/sso/{provider.provider_id}/callback", state=state)
    SsoAuthorizationAttempt.objects.create(state_digest=hashlib.sha256(state.encode()).hexdigest(),
        provider_id=provider.provider_id, provider_fingerprint=provider.fingerprint, purpose=purpose,
        actor=actor, actor_auth_hash=actor.get_session_auth_hash() if actor else "", session_digest=binding,
        return_to=return_to, expires_at=now + AUTHORIZATION_LIFETIME)
    return {"authorizationUrl": response["Location"]}


def consume_attempt(request, provider):
    states = request.GET.getlist("state")
    if len(states) != 1 or not states[0] or len(states[0]) > 512:
        raise SsoRejected("sso_state_invalid")
    digest = hashlib.sha256(states[0].encode()).hexdigest()
    with transaction.atomic():
        attempt = SsoAuthorizationAttempt.objects.select_for_update().filter(pk=digest).first()
        if (not attempt or attempt.consumed_at or attempt.expires_at <= timezone.now()
                or attempt.provider_id != provider.provider_id or attempt.provider_fingerprint != provider.fingerprint
                or not constant_time_compare(attempt.session_digest, session_digest(request, create=False))):
            raise SsoRejected("sso_state_invalid")
        if attempt.purpose == "link":
            if (not request.user.is_authenticated or request.user.pk != attempt.actor_id
                    or not constant_time_compare(request.user.get_session_auth_hash(), attempt.actor_auth_hash)):
                raise SsoRejected("sso_state_invalid")
        elif request.user.is_authenticated:
            raise SsoRejected("sso_state_invalid")
        attempt.consumed_at = timezone.now()
        attempt.save(update_fields=["consumed_at"])
    return attempt


def profile_value(profile, field):
    for part in field.split("."):
        if not isinstance(profile, dict) or part not in profile:
            return None
        profile = profile[part]
    return profile


def associate_identity(request, provider, attempt, profile):
    raw_subject = profile_value(profile, provider.subject_field)
    if isinstance(raw_subject, bool) or not isinstance(raw_subject, (str, int)):
        raise SsoRejected("sso_profile_invalid")
    subject = str(raw_subject)
    if not subject.strip() or len(subject) > 255:
        raise SsoRejected("sso_profile_invalid")
    name = profile_value(profile, provider.display_name_field)
    name = name[:150] if isinstance(name, str) else ""
    if attempt.purpose == "link":
        try:
            with transaction.atomic():
                actor = get_user_model().objects.select_for_update().get(pk=attempt.actor_id)
                if not actor.is_active or not constant_time_compare(actor.get_session_auth_hash(), attempt.actor_auth_hash):
                    raise SsoRejected("sso_state_invalid")
                existing = ExternalIdentity.objects.filter(provider_id=provider.provider_id, subject=subject).first()
                if existing:
                    if existing.user_id != actor.pk or existing.provider_fingerprint != provider.fingerprint:
                        raise SsoRejected("sso_identity_conflict", 409)
                    return existing
                return ExternalIdentity.objects.create(user=actor, provider_id=provider.provider_id,
                    provider_name=provider.display_name, provider_fingerprint=provider.fingerprint,
                    subject=subject, display_name=name)
        except IntegrityError:
            raise SsoRejected("sso_identity_conflict", 409) from None
    with transaction.atomic():
        identity = ExternalIdentity.objects.select_for_update().select_related("user").filter(
            provider_id=provider.provider_id, subject=subject).first()
        if not identity:
            raise SsoRejected("sso_identity_unlinked", 403)
        if identity.provider_fingerprint != provider.fingerprint:
            raise SsoRejected("sso_provider_changed", 403)
        if not identity.user.is_active:
            raise SsoRejected("sso_account_unavailable", 403)
        identity.last_login_at = timezone.now()
        identity.display_name = name
        identity.save(update_fields=["last_login_at", "display_name"])
        login(request, identity.user, backend=settings.AUTHENTICATION_BACKENDS[0])
        request.session["externalIdentityId"] = identity.pk
        return identity


def return_response(path, **message):
    parts = urlsplit(path)
    query = dict(parse_qsl(parts.query))
    query.update(message)
    response = HttpResponseRedirect(settings.WEB_ORIGIN + urlunsplit(("", "", parts.path, urlencode(query), parts.fragment)))
    response["Cache-Control"] = "no-store"
    response["Referrer-Policy"] = "no-referrer"
    return response


def complete_authorization(request, provider_id):
    attempt = None
    client = None
    try:
        provider = provider_for(provider_id)
        client = oauth_client(provider)
        attempt = consume_attempt(request, provider)
        if request.GET.get("error"):
            code = "sso_cancelled" if request.GET.get("error") == "access_denied" else "sso_authorization_failed"
            raise SsoRejected(code)
        if len(request.GET.getlist("code")) != 1 or not request.GET["code"]:
            raise SsoRejected("sso_state_invalid")
        token = client.authorize_access_token(request)
        result = client.get(provider.userinfo_url, token=token)
        result.raise_for_status()
        associate_identity(request, provider, attempt, result.json())
        return return_response(attempt.return_to, **({"ssoNotice": "linked"} if attempt.purpose == "link" else {}))
    except SsoRejected as error:
        code = error.code
    except Exception:
        # Provider errors can include codes, tokens, URLs or client credentials.
        # Expose a stable error, never an upstream exception or response body.
        code = "sso_authorization_failed"
    finally:
        if client and request.GET.get("state"):
            client.framework.clear_state_data(request.session, request.GET["state"])
    destination = attempt.return_to if attempt and attempt.purpose == "link" else "/login"
    return return_response(destination, ssoError=code)


def external_session_valid(request):
    identity_id = request.session.get("externalIdentityId")
    if not identity_id:
        return True
    identity = ExternalIdentity.objects.filter(pk=identity_id, user_id=request.user.pk).first()
    if identity:
        provider = next((p for p in providers() if p.provider_id == identity.provider_id), None)
        if provider and provider.fingerprint == identity.provider_fingerprint:
            return True
    logout(request)
    return False
