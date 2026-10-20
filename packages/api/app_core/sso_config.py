"""Deployment-owned OAuth client configuration; no account policy lives here."""
import hashlib
import json
from urllib.parse import urlsplit

from pydantic import BaseModel, ConfigDict, Field, SecretStr
from pydantic.alias_generators import to_camel


class OAuthProvider(BaseModel):
    model_config = ConfigDict(alias_generator=to_camel, extra="forbid", strict=True, validate_by_name=False)
    provider_id: str = Field(pattern=r"^[a-z][a-z0-9_-]{0,63}$")
    display_name: str = Field(min_length=1, max_length=160)
    client_id: str = Field(min_length=1)
    client_secret: SecretStr
    authorize_url: str
    token_url: str
    userinfo_url: str
    scope: str = ""
    subject_field: str
    display_name_field: str = "name"
    token_endpoint_auth_method: str = "client_secret_basic"

    @property
    def fingerprint(self):
        # Labels and secret rotation do not change the external identity namespace.
        registration = [self.client_id, self.authorize_url, self.token_url, self.userinfo_url, self.subject_field]
        return hashlib.sha256(json.dumps(registration, separators=(",", ":")).encode()).hexdigest()


def validate_providers(values, *, allow_insecure_http=False):
    try:
        if not isinstance(values, (list, tuple)):
            raise ValueError
        providers = tuple(OAuthProvider.model_validate(value) for value in values)
        if len({p.provider_id for p in providers}) != len(providers):
            raise ValueError
        for provider in providers:
            if (not provider.display_name.strip() or not provider.client_secret.get_secret_value()
                    or provider.token_endpoint_auth_method not in {"client_secret_basic", "client_secret_post"}
                    or "openid" in provider.scope.split()):
                # OIDC identity validation will be added and accepted as a separate boundary.
                raise ValueError
            for field in [provider.subject_field, provider.display_name_field]:
                if not field or any(not part for part in field.split(".")):
                    raise ValueError
            for value in [provider.authorize_url, provider.token_url, provider.userinfo_url]:
                parsed = urlsplit(value)
                if (parsed.scheme not in ({"https", "http"} if allow_insecure_http else {"https"})
                        or not parsed.hostname or parsed.username or parsed.password or parsed.fragment):
                    raise ValueError
                _ = parsed.port
        return providers
    except (ValueError, TypeError):
        # Validation errors must never include the input object or client secret.
        raise RuntimeError("Invalid OAuth provider configuration") from None


def parse_providers(value, *, allow_insecure_http=False):
    try:
        values = json.loads(value)
    except (ValueError, TypeError):
        raise RuntimeError("Invalid OAuth provider configuration") from None
    return validate_providers(values, allow_insecure_http=allow_insecure_http)
