import secrets
import unicodedata
import uuid
from urllib.parse import urlsplit

from django.conf import settings
from django.contrib.sessions.base_session import AbstractBaseSession
from django.core.exceptions import ValidationError
from django.core.validators import validate_email
from django.db import IntegrityError, models, transaction
from .session_payload import SessionPayloadField

from .agent_identity import (
    normalize_agent_description,
    normalize_agent_instructions,
    normalize_agent_name,
    validate_agent_id,
)
from .credentials import validate_display_name, validate_lower_kebab
from .app_delegation_contract import AUDIENCE, ISSUER, validate_scopes


DEFAULT_MODEL_CONTEXT_TOKENS = 200_000
DEFAULT_MODEL_OUTPUT_TOKENS = 32_768
WORKSPACE_ROLES = frozenset({"owner", "admin", "member"})
WORKSPACE_INVITATION_ROLES = frozenset({"admin", "member"})
WORKSPACE_INVITATION_STATUSES = frozenset(
    {"pending", "accepted", "revoked", "expired"}
)
PASSWORD_RESET_MAIL_STATUSES = frozenset({"pending", "sent", "suppressed"})
WORKSPACE_GROUP_KINDS = frozenset({"custom", "all_members"})
SOURCE_ACCESS_LEVELS = frozenset({"read", "write", "control"})
MODEL_API_IDS = frozenset(
    {"openai-completions", "openai-responses", "anthropic-messages"}
)


def new_id(prefix: str) -> str:
    return f"{prefix}_{uuid.uuid4().hex}"


def new_workspace_id() -> str:
    return f"ws_{secrets.token_urlsafe(12)}"


def new_workspace_membership_id() -> str:
    return new_id("wsm")


def new_workspace_invitation_id() -> str:
    return new_id("wsi")


def new_model_id() -> str:
    return new_id("model")


def new_provider_id() -> str:
    return new_id("provider")


def new_credential_id() -> str:
    return new_id("credential")


def new_credential_audit_id() -> str:
    return new_id("credential_audit")


def new_mcp_bearer_credential_id() -> str:
    return new_id("mcp_credential")


def new_mcp_credential_audit_id() -> str:
    return new_id("mcp_credential_audit")


def new_session_id() -> str:
    return f"session_{secrets.token_urlsafe(12)}"


def new_session_project_id() -> str:
    return new_id("session_project")


def new_agent_id() -> str:
    return f"agent_{secrets.token_urlsafe(12)}"


def new_agent_definition_id() -> str:
    return new_id("agent_definition")


def new_agent_definition_version_id() -> str:
    return new_id("agent_definition_version")


def new_agent_connector_approval_id() -> str:
    return new_id("connector_approval")


def new_agent_connector_binding_id() -> str:
    return new_id("connector_binding")


def new_business_app_id() -> str:
    return new_id("business_app")


def new_app_delegation_id() -> str:
    return new_id("app_delegation")


def new_business_agent_branch_id() -> str:
    return new_id("business_branch")


def empty_plugin_activation() -> dict:
    from .plugin_catalog import PLUGIN_ACTIVATION_SCHEMA, activation_digest
    return {"schema": PLUGIN_ACTIVATION_SCHEMA, "digest": activation_digest([]), "packages": []}


def new_agent_run_id() -> str:
    return new_id("agent_run")


def new_turn_id() -> str:
    return new_id("turn")


def new_agent_run_authorization_id() -> str:
    return new_id("agent_run_authorization")


def new_model_run_id() -> str:
    return new_id("model_run")


def new_artifact_id() -> str:
    return new_id("art")


def new_source_id() -> str:
    return new_id("src")


def new_source_object_id() -> str:
    return new_id("srcobj")


def new_source_grant_id() -> str:
    return new_id("grant")


def new_workspace_group_id() -> str:
    return new_id("wsgroup")


def new_derived_resource_id() -> str:
    return new_id("derived")


def new_library_object_id() -> str:
    return new_id("libobj")


def new_library_link_id() -> str:
    return new_id("liblink")


def new_session_asset_link_id() -> str:
    return new_id("assetlink")


def require_enum(name: str, value: str, allowed: set[str] | tuple[str, ...]) -> None:
    if value not in allowed:
        raise ValueError(f"unsupported {name}: {value}")


def validate_thinking_mode(value: str) -> None:
    if (
        not isinstance(value, str)
        or not value
        or value != value.strip()
        or len(value) > 64
        or unicodedata.normalize("NFC", value) != value
        or any(unicodedata.category(character).startswith("C") for character in value)
    ):
        raise ValueError("thinking mode is invalid")


def validate_thinking_config(thinking_mode: str, thinking_modes: list[str]) -> None:
    if not isinstance(thinking_modes, list) or len(thinking_modes) > 16:
        raise ValueError("thinkingModes must be a list of at most 16 values")
    for value in thinking_modes:
        validate_thinking_mode(value)
    if len(set(thinking_modes)) != len(thinking_modes):
        raise ValueError("thinkingModes must be unique")
    if thinking_mode:
        validate_thinking_mode(thinking_mode)
        if thinking_mode not in thinking_modes:
            raise ValueError("thinkingMode must be listed in thinkingModes")


def require_failure_reason(status: str, failure_reason: str) -> None:
    if status == "failed" and not failure_reason.strip():
        raise ValueError("failed status requires failureReason")


def require_sha256(name: str, value: str) -> None:
    digest = value.removeprefix("sha256:") if isinstance(value, str) else ""
    if (
        not isinstance(value, str)
        or not value.startswith("sha256:")
        or len(digest) != 64
        or any(character not in "0123456789abcdef" for character in digest)
    ):
        raise ValueError(f"{name} must be sha256:<64 lowercase hex>")


def normalize_workspace_invitation_email(email: str) -> str:
    normalized = email.strip().lower() if isinstance(email, str) else ""
    try:
        validate_email(normalized)
    except ValidationError as error:
        raise ValueError("WorkspaceInvitation email is invalid") from error
    return normalized


class ModelEndpointValidationError(ValueError):
    def __init__(self, code: str):
        super().__init__(code)
        self.code = code


def validate_model_endpoint(
    api_base: str,
) -> None:
    if not isinstance(api_base, str) or not api_base or api_base != api_base.strip():
        raise ModelEndpointValidationError("model_endpoint_invalid")
    parsed = urlsplit(api_base)
    try:
        port = parsed.port
    except ValueError as error:
        raise ModelEndpointValidationError("model_endpoint_invalid") from error
    if (
        parsed.scheme not in {"http", "https"}
        or not parsed.hostname
        or any(character.isspace() for character in parsed.hostname)
        or "\\" in parsed.hostname
        or (port is not None and not 0 < port <= 65535)
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
    ):
        raise ModelEndpointValidationError("model_endpoint_invalid")
    if parsed.scheme != "https":
        raise ModelEndpointValidationError("model_endpoint_https_required")


class ResourceQuerySet(models.QuerySet):
    def create(self, **kwargs):
        if "id" in kwargs or "pk" in kwargs:
            return super().create(**kwargs)
        self._for_write = True
        for attempt in range(3):
            try:
                # A savepoint keeps the caller's transaction usable after a collision.
                with transaction.atomic(using=self.db):
                    return super().create(**kwargs)
            except IntegrityError as error:
                cause = error.__cause__
                primary_key_conflict = (
                    getattr(cause, "sqlite_errorname", None) == "SQLITE_CONSTRAINT_PRIMARYKEY"
                    or (
                        getattr(cause, "sqlstate", None) == "23505"
                        and getattr(getattr(cause, "diag", None), "constraint_name", None)
                        == f"{self.model._meta.db_table}_pkey"
                    )
                )
                if not primary_key_conflict or attempt == 2:
                    raise


class Workspace(models.Model):
    objects = ResourceQuerySet.as_manager()
    id = models.CharField(primary_key=True, max_length=64, default=new_workspace_id)
    name = models.CharField(max_length=160)
    description = models.TextField(blank=True, default="")
    status = models.CharField(max_length=32, default="active")
    createdBy = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.PROTECT,
        related_name="created_workspaces",
    )
    members = models.ManyToManyField(
        settings.AUTH_USER_MODEL,
        through="WorkspaceMembership",
        through_fields=("workspace", "user"),
        related_name="workspaces",
    )
    createdAt = models.DateTimeField(auto_now_add=True)
    updatedAt = models.DateTimeField(auto_now=True)

    def save(self, *args, **kwargs):
        require_enum("Workspace.status", self.status, {"active", "archived"})
        return super().save(*args, **kwargs)


class WorkspaceMembership(models.Model):
    id = models.CharField(
        primary_key=True,
        max_length=64,
        default=new_workspace_membership_id,
    )
    workspace = models.ForeignKey(
        Workspace,
        on_delete=models.CASCADE,
        related_name="memberships",
    )
    user = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.PROTECT,
        related_name="workspace_memberships",
    )
    role = models.CharField(max_length=16, default="member")
    invited_by = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.PROTECT,
        related_name="invited_workspace_memberships",
        null=True,
        blank=True,
    )
    created_at = models.DateTimeField(auto_now_add=True)
    updated_at = models.DateTimeField(auto_now=True)

    class Meta:
        constraints = [
            models.UniqueConstraint(
                fields=["workspace", "user"],
                name="unique_workspace_membership",
            ),
            models.UniqueConstraint(
                fields=["workspace"],
                condition=models.Q(role="owner"),
                name="unique_workspace_owner",
            ),
            models.CheckConstraint(
                condition=models.Q(role__in=("owner", "admin", "member")),
                name="workspace_membership_role_valid",
            ),
        ]

    def save(self, *args, **kwargs):
        require_enum(
            "WorkspaceMembership.role",
            self.role,
            WORKSPACE_ROLES,
        )
        return super().save(*args, **kwargs)


class WorkspaceInvitation(models.Model):
    id = models.CharField(
        primary_key=True,
        max_length=64,
        default=new_workspace_invitation_id,
    )
    workspace = models.ForeignKey(
        Workspace,
        on_delete=models.CASCADE,
        related_name="invitations",
    )
    email = models.EmailField(max_length=254)
    role = models.CharField(max_length=16)
    status = models.CharField(max_length=16, default="pending")
    token_digest = models.CharField(max_length=71, unique=True, editable=False)
    invited_by = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.PROTECT,
        related_name="issued_workspace_invitations",
    )
    accepted_by = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.PROTECT,
        related_name="accepted_workspace_invitations",
        null=True,
        blank=True,
    )
    accepted_membership_ref = models.CharField(max_length=64, blank=True, default="")
    revoked_by = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.PROTECT,
        related_name="revoked_workspace_invitations",
        null=True,
        blank=True,
    )
    expires_at = models.DateTimeField()
    created_at = models.DateTimeField(auto_now_add=True)
    updated_at = models.DateTimeField(auto_now=True)

    class Meta:
        constraints = [
            models.UniqueConstraint(
                fields=["workspace", "email"],
                condition=models.Q(status="pending"),
                name="unique_pending_workspace_invitation",
            ),
            models.CheckConstraint(
                condition=models.Q(role__in=("admin", "member")),
                name="workspace_invitation_role_valid",
            ),
            models.CheckConstraint(
                condition=models.Q(
                    status__in=("pending", "accepted", "revoked", "expired")
                ),
                name="workspace_invitation_status_valid",
            ),
        ]

    def save(self, *args, **kwargs):
        self.email = normalize_workspace_invitation_email(self.email)
        require_enum("WorkspaceInvitation.role", self.role, WORKSPACE_INVITATION_ROLES)
        require_enum(
            "WorkspaceInvitation.status",
            self.status,
            WORKSPACE_INVITATION_STATUSES,
        )
        require_sha256("WorkspaceInvitation.token_digest", self.token_digest)
        if self.status == "pending" and (
            self.accepted_by_id
            or self.accepted_membership_ref
            or self.revoked_by_id
        ):
            raise ValueError("pending WorkspaceInvitation has terminal fields")
        if self.status == "accepted" and (
            not self.accepted_by_id or not self.accepted_membership_ref
        ):
            raise ValueError("accepted WorkspaceInvitation requires membership identity")
        if self.status == "revoked" and not self.revoked_by_id:
            raise ValueError("revoked WorkspaceInvitation requires revoker identity")
        return super().save(*args, **kwargs)


class PasswordResetMail(models.Model):
    user = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.PROTECT,
        related_name="password_reset_mails",
        null=True,
        blank=True,
    )
    email_digest = models.CharField(max_length=71)
    ip_digest = models.CharField(max_length=71)
    password_state_digest = models.CharField(max_length=71, blank=True, default="")
    status = models.CharField(max_length=16, default="pending")
    attempt_count = models.PositiveIntegerField(default=0)
    next_attempt_at = models.DateTimeField()
    last_attempt_at = models.DateTimeField(null=True, blank=True)
    last_error_kind = models.CharField(max_length=160, blank=True, default="")
    requested_at = models.DateTimeField(auto_now_add=True)
    sent_at = models.DateTimeField(null=True, blank=True)

    class Meta:
        constraints = [
            models.CheckConstraint(
                condition=models.Q(status__in=("pending", "sent", "suppressed")),
                name="password_reset_mail_status_valid",
            ),
            models.UniqueConstraint(
                fields=["email_digest"],
                condition=models.Q(status="pending"),
                name="unique_pending_password_reset_mail",
            ),
        ]
        indexes = [
            models.Index(
                fields=["status", "next_attempt_at"],
                name="password_reset_mail_due",
            ),
            models.Index(
                fields=["email_digest", "requested_at"],
                name="password_reset_mail_email_rate",
            ),
            models.Index(
                fields=["ip_digest", "requested_at"],
                name="password_reset_mail_ip_rate",
            ),
        ]

    def save(self, *args, **kwargs):
        require_enum(
            "PasswordResetMail.status",
            self.status,
            PASSWORD_RESET_MAIL_STATUSES,
        )
        require_sha256("PasswordResetMail.email_digest", self.email_digest)
        require_sha256("PasswordResetMail.ip_digest", self.ip_digest)
        if self.password_state_digest:
            require_sha256(
                "PasswordResetMail.password_state_digest",
                self.password_state_digest,
            )
        if self.status == "pending" and (
            not self.user_id or not self.password_state_digest or self.sent_at
        ):
            raise ValueError("pending PasswordResetMail requires an unsent user state")
        if self.status == "sent" and not self.sent_at:
            raise ValueError("sent PasswordResetMail requires sent_at")
        return super().save(*args, **kwargs)


class WorkspacePluginEnablement(models.Model):
    workspace = models.ForeignKey(
        Workspace, on_delete=models.CASCADE, related_name="pluginEnablements"
    )
    pluginName = models.CharField(max_length=64)
    createdAt = models.DateTimeField(auto_now_add=True)

    class Meta:
        constraints = [
            models.UniqueConstraint(
                fields=["workspace", "pluginName"],
                name="unique_workspace_plugin_enablement",
            )
        ]


class WorkspaceGroup(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_workspace_group_id)
    workspace = models.ForeignKey(
        Workspace, on_delete=models.PROTECT, related_name="groups"
    )
    name = models.CharField(max_length=160)
    kind = models.CharField(max_length=32, default="custom")
    members = models.ManyToManyField(
        WorkspaceMembership,
        related_name="workspace_groups",
    )
    createdBy = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT)
    createdAt = models.DateTimeField(auto_now_add=True)

    class Meta:
        constraints = [
            models.UniqueConstraint(
                fields=["workspace", "name"],
                name="unique_workspace_group_name",
            ),
            models.UniqueConstraint(
                fields=["workspace"],
                condition=models.Q(kind="all_members"),
                name="unique_workspace_all_members_group",
            ),
            models.CheckConstraint(
                condition=models.Q(kind__in=("custom", "all_members")),
                name="workspace_group_kind_valid",
            ),
        ]

    def save(self, *args, **kwargs):
        require_enum("WorkspaceGroup.kind", self.kind, WORKSPACE_GROUP_KINDS)
        return super().save(*args, **kwargs)


class ModelProvider(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_provider_id)
    displayName = models.CharField(max_length=160)
    template_id = models.CharField(max_length=64, null=True, blank=True)
    api = models.CharField(max_length=32)
    apiBase = models.CharField(max_length=512)
    enabled = models.BooleanField(default=True)
    archivedAt = models.DateTimeField(null=True, blank=True)
    createdAt = models.DateTimeField(auto_now_add=True)
    updatedAt = models.DateTimeField(auto_now=True)

    class Meta:
        constraints = [
            models.UniqueConstraint(
                fields=["template_id"],
                condition=models.Q(template_id__isnull=False, archivedAt__isnull=True),
                name="model_provider_active_template_unique",
            ),
        ]

    def save(self, *args, **kwargs):
        if not self.displayName.strip():
            raise ValueError("ModelProvider displayName is required")
        if self.template_id is not None and not self.template_id.strip():
            raise ValueError("ModelProvider template_id must be null or non-empty")
        require_enum("ModelProvider.api", self.api, MODEL_API_IDS)
        validate_model_endpoint(self.apiBase)
        if self.archivedAt is not None and self.enabled:
            raise ValueError("archived ModelProvider must be disabled")
        return super().save(*args, **kwargs)


class ModelConfig(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_model_id)
    familyId = models.CharField(max_length=64, default=new_model_id)
    revision = models.PositiveIntegerField(default=1)
    isCurrent = models.BooleanField(default=True)
    displayName = models.CharField(max_length=160, blank=True, default="")
    provider = models.ForeignKey(
        ModelProvider,
        on_delete=models.PROTECT,
        related_name="models",
        null=True,
        blank=True,
    )
    modelName = models.CharField(max_length=160, default="fake-model")
    apiOverride = models.CharField(max_length=32, null=True, blank=True)
    resolvedApi = models.CharField(max_length=32, blank=True, default="")
    resolvedApiBase = models.CharField(max_length=512, blank=True, default="")
    contextTokens = models.PositiveIntegerField(default=DEFAULT_MODEL_CONTEXT_TOKENS)
    maxOutputTokens = models.PositiveIntegerField(default=DEFAULT_MODEL_OUTPUT_TOKENS)
    thinkingMode = models.CharField(max_length=64, blank=True, default="")
    thinkingModes = models.JSONField(default=list)
    enabled = models.BooleanField(default=True)
    createdAt = models.DateTimeField(auto_now_add=True)
    updatedAt = models.DateTimeField(auto_now=True)

    class Meta:
        constraints = [
            models.UniqueConstraint(
                fields=["familyId", "revision"],
                name="model_config_family_revision_unique",
            ),
            models.UniqueConstraint(
                fields=["provider", "modelName"],
                condition=models.Q(isCurrent=True, provider__isnull=False),
                name="model_config_current_provider_model_unique",
            ),
        ]

    def save(self, *args, **kwargs):
        if not self.modelName.strip():
            raise ValueError("ModelConfig modelName is required")
        if self.contextTokens <= 0 or self.maxOutputTokens <= 0:
            raise ValueError("ModelConfig token limits must be positive")
        if self.maxOutputTokens >= self.contextTokens:
            raise ValueError(
                "ModelConfig maxOutputTokens must be smaller than contextTokens"
            )
        validate_thinking_config(self.thinkingMode, self.thinkingModes)
        if self.provider_id is None:
            if self.apiOverride or self.resolvedApi or self.resolvedApiBase:
                raise ValueError("fake ModelConfig must not define provider settings")
            return super().save(*args, **kwargs)
        provider = self.provider
        if self.apiOverride is not None:
            require_enum("ModelConfig.apiOverride", self.apiOverride, MODEL_API_IDS)
        if not self.resolvedApi:
            self.resolvedApi = self.apiOverride or provider.api
        if not self.resolvedApiBase:
            self.resolvedApiBase = provider.apiBase
        require_enum("ModelConfig.resolvedApi", self.resolvedApi, MODEL_API_IDS)
        validate_model_endpoint(self.resolvedApiBase)
        return super().save(*args, **kwargs)


class ModelQuotaDomain(models.Model):
    # Quota identity and limit are configured explicitly for the external
    # account. Neither provider names nor secrets establish this relationship.
    key = models.AutoField(primary_key=True)
    id = models.CharField(unique=True, max_length=64)
    maxConcurrent = models.PositiveSmallIntegerField()
    enabled = models.BooleanField(default=False)
    cooldownUntilMs = models.BigIntegerField(default=0)
    createdAt = models.DateTimeField(auto_now_add=True)
    updatedAt = models.DateTimeField(auto_now=True)

    class Meta:
        constraints = [
            models.CheckConstraint(
                condition=models.Q(maxConcurrent__gte=1, maxConcurrent__lte=64),
                name="model_quota_domain_concurrency_valid",
            ),
        ]

    def save(self, *args, **kwargs):
        if not self.id.strip() or self.maxConcurrent < 1 or self.maxConcurrent > 64:
            raise ValueError("ModelQuotaDomain requires an ID and a limit from 1 to 64")
        return super().save(*args, **kwargs)


class ProviderCredential(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_credential_id)
    provider = models.OneToOneField(
        ModelProvider,
        on_delete=models.PROTECT,
        related_name="credential",
    )
    quotaDomain = models.ForeignKey(
        ModelQuotaDomain,
        null=True,
        blank=True,
        on_delete=models.PROTECT,
        related_name="credentials",
    )
    displayName = models.CharField(max_length=160)
    encryptedSecret = models.TextField()
    version = models.PositiveIntegerField(default=1)
    createdBy = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.PROTECT,
        related_name="createdCredentials",
    )
    updatedBy = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.PROTECT,
        related_name="updatedCredentials",
    )
    createdAt = models.DateTimeField(auto_now_add=True)
    updatedAt = models.DateTimeField(auto_now=True)

    class Meta:
        constraints = []

    def save(self, *args, **kwargs):
        if not self.displayName.strip() or not self.encryptedSecret:
            raise ValueError(
                "ProviderCredential displayName and encryptedSecret are required"
            )
        return super().save(*args, **kwargs)


class CredentialAuditEvent(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_credential_audit_id)
    credentialId = models.CharField(max_length=64)
    provider = models.CharField(max_length=80)
    displayName = models.CharField(max_length=160)
    action = models.CharField(max_length=32)
    actor = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT)
    createdAt = models.DateTimeField(auto_now_add=True)

    def save(self, *args, **kwargs):
        require_enum(
            "CredentialAuditEvent.action",
            self.action,
            {"created", "rotated", "deleted", "tested"},
        )
        return super().save(*args, **kwargs)


class BusinessApplication(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_business_app_id)
    name = models.CharField(max_length=160)
    status = models.CharField(max_length=16, default="pending")
    created_by = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT, related_name="registered_business_apps")
    created_at = models.DateTimeField(auto_now_add=True)
    updated_at = models.DateTimeField(auto_now=True)

    class Meta:
        constraints = [models.CheckConstraint(condition=models.Q(status__in=("pending", "active", "revoked")),
                                              name="business_app_status_valid")]

    def save(self, *args, **kwargs):
        validate_display_name(self.name)
        require_enum("BusinessApplication.status", self.status, {"pending", "active", "revoked"})
        if not self._state.adding:
            stored = type(self).objects.values_list("status", flat=True).get(pk=self.pk)
            if (stored == "revoked" and self.status != stored) or (stored == "active" and self.status == "pending"):
                raise ValueError("business_app_revoked")
        return super().save(*args, **kwargs)


class UserAppDelegation(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_app_delegation_id)
    user = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT, related_name="app_delegations")
    app = models.ForeignKey(BusinessApplication, on_delete=models.PROTECT, related_name="delegations")
    workspace = models.ForeignKey(Workspace, on_delete=models.PROTECT, related_name="app_delegations")
    definition = models.ForeignKey("AgentDefinition", on_delete=models.PROTECT, related_name="app_delegations", null=True, blank=True)
    agent = models.ForeignKey("Agent", on_delete=models.PROTECT, related_name="app_delegations", null=True, blank=True)
    membership_ref = models.CharField(max_length=64, editable=False)
    issuer = models.CharField(max_length=100, default=ISSUER, editable=False)
    audience = models.CharField(max_length=100, default=AUDIENCE, editable=False)
    scopes = models.JSONField()
    token_digest = models.CharField(max_length=71, unique=True, editable=False)
    credential_version = models.PositiveBigIntegerField(default=1, editable=False)
    expires_at = models.DateTimeField(null=True, blank=True)
    revoked_at = models.DateTimeField(null=True, blank=True)
    created_at = models.DateTimeField(auto_now_add=True)

    class Meta:
        constraints = [
            models.CheckConstraint(condition=~models.Q(membership_ref=""), name="app_delegation_membership_required"),
            models.CheckConstraint(condition=(models.Q(definition__isnull=False, agent__isnull=True)
                | models.Q(definition__isnull=True, agent__isnull=False)), name="app_delegation_target_exactly_one"),
            models.CheckConstraint(condition=models.Q(credential_version__gt=0), name="app_delegation_version_positive"),
        ]

    def save(self, *args, **kwargs):
        from .app_delegation_contract import NATIVE_SCOPES
        self.scopes = validate_scopes(self.scopes)
        require_sha256("UserAppDelegation.token_digest", self.token_digest)
        if (self.definition_id is None) == (self.agent_id is None):
            raise ValueError("UserAppDelegation requires exactly one target")
        target_matches = (self.definition.workspace_id == self.workspace_id if self.definition_id else
            self.agent.workspace_id == self.workspace_id and self.agent.owner_id == self.user_id and self.agent.definition_id is None)
        if self.issuer != ISSUER or self.audience != AUDIENCE or not target_matches:
            raise ValueError("UserAppDelegation authority binding mismatch")
        if self.agent_id and not set(self.scopes) <= NATIVE_SCOPES:
            raise ValueError("UserAppDelegation native scopes are invalid")
        if type(self.credential_version) is not int or self.credential_version <= 0:
            raise ValueError("UserAppDelegation credential version is invalid")
        fields = ("user_id", "app_id", "workspace_id", "definition_id", "agent_id", "membership_ref", "issuer", "audience",
                  "scopes", "token_digest", "credential_version", "expires_at")
        if self._state.adding:
            if self.credential_version != 1:
                raise ValueError("UserAppDelegation initial credential version must be one")
            if self.agent_id and self.agent.status != "active":
                raise ValueError("UserAppDelegation native Agent must be active")
            if not WorkspaceMembership.objects.filter(id=self.membership_ref, workspace_id=self.workspace_id,
                user_id=self.user_id, workspace__status="active", role__in=WORKSPACE_ROLES).exists():
                raise ValueError("UserAppDelegation requires current membership")
        else:
            stored = type(self).objects.values(*fields, "revoked_at").get(pk=self.pk)
            if any(stored[field] != getattr(self, field) for field in fields):
                raise ValueError("UserAppDelegation authority is immutable")
            if stored["revoked_at"] is not None and self.revoked_at != stored["revoked_at"]:
                raise ValueError("UserAppDelegation revocation is permanent")
        return super().save(*args, **kwargs)


class AppDelegationAuditEvent(models.Model):
    """Append-only security metadata, without bearer values or digests."""
    delegation = models.ForeignKey(UserAppDelegation, on_delete=models.PROTECT, related_name="audit_events")
    actor = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT)
    action = models.CharField(max_length=16)
    credential_version = models.PositiveBigIntegerField()
    created_at = models.DateTimeField(auto_now_add=True)

    class Meta:
        constraints = [
            models.CheckConstraint(condition=models.Q(action__in=("issued", "rotated", "revoked")), name="app_delegation_audit_action_valid"),
            models.CheckConstraint(condition=models.Q(credential_version__gt=0), name="app_delegation_audit_version_positive"),
        ]

    def save(self, *args, **kwargs):
        if not self._state.adding:
            raise ValueError("app_delegation_audit_is_immutable")
        require_enum("AppDelegationAuditEvent.action", self.action, {"issued", "rotated", "revoked"})
        if (type(self.credential_version) is not int or self.credential_version <= 0
                or self.credential_version > self.delegation.credential_version
                or self.actor_id != self.delegation.user_id):
            raise ValueError("app_delegation_audit_binding_rejected")
        kwargs["force_insert"] = True
        return super().save(*args, **kwargs)


class AgentDefinition(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_agent_definition_id)
    workspace = models.ForeignKey(Workspace, on_delete=models.PROTECT, related_name="agent_definitions")
    name = models.CharField(max_length=255)
    description = models.CharField(max_length=128, blank=True, default="")
    instructions = models.TextField(blank=True, default="")
    avatar_kind = models.CharField(max_length=16, default="centaeris")
    plugin_names = models.JSONField(default=list)
    status = models.CharField(max_length=16, default="active")
    availability_scope = models.CharField(max_length=16, default="none")
    published_version = models.ForeignKey(
        "AgentDefinitionVersion", on_delete=models.PROTECT, related_name="+", null=True, blank=True,
    )
    created_by = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT, related_name="+")
    created_at = models.DateTimeField(auto_now_add=True)
    updated_at = models.DateTimeField(auto_now=True)

    class Meta:
        constraints = [
            models.CheckConstraint(condition=models.Q(status__in=("active", "disabled")), name="agent_definition_status_valid"),
            models.CheckConstraint(condition=models.Q(availability_scope__in=("none", "workspace", "members")), name="agent_definition_scope_valid"),
            models.CheckConstraint(condition=models.Q(avatar_kind__in=("centaeris", "banana")), name="agent_definition_avatar_valid"),
        ]

    def save(self, *args, **kwargs):
        from .plugin_catalog import validate_plugin_names
        validate_plugin_names(self.plugin_names)
        self.name = normalize_agent_name(self.name)
        self.description = normalize_agent_description(self.description)
        self.instructions = normalize_agent_instructions(self.instructions)
        require_enum("AgentDefinition.avatar_kind", self.avatar_kind, {"centaeris", "banana"})
        require_enum("AgentDefinition.status", self.status, {"active", "disabled"})
        require_enum("AgentDefinition.availability_scope", self.availability_scope, {"none", "workspace", "members"})
        if self.published_version_id and self.published_version.definition_id != self.id:
            raise ValueError("AgentDefinition published version binding mismatch")
        if not self._state.adding:
            if type(self).objects.values_list("workspace_id", flat=True).get(pk=self.pk) != self.workspace_id:
                raise ValueError("AgentDefinition workspace is immutable")
        return super().save(*args, **kwargs)


class ImmutableDefinitionVersionQuerySet(models.QuerySet):
    def update(self, **kwargs):
        raise ValueError("AgentDefinitionVersion is immutable")

    def delete(self):
        raise ValueError("AgentDefinitionVersion is immutable")

    def bulk_create(self, objs, batch_size=None, ignore_conflicts=False, update_conflicts=False,
                    update_fields=None, unique_fields=None):
        if update_conflicts:
            raise ValueError("AgentDefinitionVersion is immutable")
        return super().bulk_create(objs, batch_size=batch_size, ignore_conflicts=ignore_conflicts,
                                   update_conflicts=False, update_fields=update_fields, unique_fields=unique_fields)


class AgentDefinitionVersion(models.Model):
    objects = ImmutableDefinitionVersionQuerySet.as_manager()
    id = models.CharField(primary_key=True, max_length=64, default=new_agent_definition_version_id)
    definition = models.ForeignKey(AgentDefinition, on_delete=models.PROTECT, related_name="versions")
    version = models.PositiveIntegerField()
    name = models.CharField(max_length=255)
    description = models.CharField(max_length=128, blank=True, default="")
    instructions = models.TextField(blank=True, default="")
    avatar_kind = models.CharField(max_length=16, default="centaeris")
    plugin_activation = models.JSONField(default=empty_plugin_activation)
    published_by = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT, related_name="+")
    published_at = models.DateTimeField(auto_now_add=True)

    class Meta:
        constraints = [
            models.UniqueConstraint(fields=["definition", "version"], name="agent_definition_version_unique"),
            models.CheckConstraint(condition=models.Q(version__gt=0), name="agent_definition_version_positive"),
            models.CheckConstraint(condition=models.Q(avatar_kind__in=("centaeris", "banana")), name="agent_definition_version_avatar"),
        ]

    def save(self, *args, **kwargs):
        from .plugin_catalog import validate_plugin_activation
        validate_plugin_activation(self.plugin_activation)
        if not self._state.adding:
            raise ValueError("AgentDefinitionVersion is immutable")
        self.name = normalize_agent_name(self.name)
        self.description = normalize_agent_description(self.description)
        self.instructions = normalize_agent_instructions(self.instructions)
        require_enum("AgentDefinitionVersion.avatar_kind", self.avatar_kind, {"centaeris", "banana"})
        if self.version <= 0:
            raise ValueError("AgentDefinitionVersion version must be positive")
        return super().save(*args, **kwargs)

    def delete(self, *args, **kwargs):
        raise ValueError("AgentDefinitionVersion is immutable")


class AgentDefinitionMember(models.Model):
    definition = models.ForeignKey(AgentDefinition, on_delete=models.PROTECT, related_name="member_grants")
    membership = models.ForeignKey(WorkspaceMembership, on_delete=models.CASCADE, related_name="agent_definition_grants")

    class Meta:
        constraints = [models.UniqueConstraint(fields=["definition", "membership"], name="agent_definition_member_unique")]

    def save(self, *args, **kwargs):
        if self.definition.workspace_id != self.membership.workspace_id:
            raise ValueError("AgentDefinitionMember workspace binding mismatch")
        return super().save(*args, **kwargs)


class Agent(models.Model):
    is_business_instance = models.BooleanField(default=False)
    objects = ResourceQuerySet.as_manager()
    id = models.CharField(primary_key=True, max_length=64, default=new_agent_id)
    workspace = models.ForeignKey(
        Workspace,
        on_delete=models.PROTECT,
        related_name="agents",
    )
    owner = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.PROTECT,
        related_name="agents",
    )
    definition = models.ForeignKey(
        AgentDefinition, on_delete=models.PROTECT, related_name="instances", null=True, blank=True,
    )
    name = models.CharField(max_length=255)
    description = models.CharField(max_length=128, blank=True, default="")
    instructions = models.TextField(blank=True, default="")
    avatar_kind = models.CharField(max_length=16, default="centaeris")
    status = models.CharField(max_length=16, default="active")
    model_config = models.ForeignKey(ModelConfig, on_delete=models.PROTECT, null=True, blank=True, related_name="configured_agents")
    thinking_mode = models.CharField(max_length=64, blank=True, default="")
    deletedAt = models.DateTimeField(null=True, blank=True)
    deletedBy = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.PROTECT,
        related_name="+",
        null=True,
        blank=True,
    )
    purgedAt = models.DateTimeField(null=True, blank=True)
    createdAt = models.DateTimeField(auto_now_add=True)
    updatedAt = models.DateTimeField(auto_now=True)

    class Meta:
        constraints = [
            models.UniqueConstraint(fields=["workspace", "owner", "definition"],
                condition=models.Q(is_business_instance=False), name="agent_definition_owner_unique"),
            models.CheckConstraint(condition=models.Q(model_config__isnull=False) | models.Q(thinking_mode=""),
                name="agent_unconfigured_model_no_effort"),
            models.CheckConstraint(
                condition=models.Q(avatar_kind__in=("centaeris", "banana")),
                name="agent_avatar_kind_valid",
            ),
            models.CheckConstraint(
                condition=models.Q(status__in=("active", "deleted")),
                name="agent_status_valid",
            ),
            models.CheckConstraint(
                condition=(
                    models.Q(
                        status="active",
                        deletedAt__isnull=True,
                        purgedAt__isnull=True,
                    )
                    | (
                        models.Q(status="deleted", deletedAt__isnull=False)
                        & (
                            models.Q(purgedAt__isnull=True)
                            | models.Q(purgedAt__gte=models.F("deletedAt"))
                        )
                    )
                ),
                name="agent_deletion_consistency",
            ),
        ]

    def save(self, *args, **kwargs):
        validate_agent_id(self.id)
        self.name = normalize_agent_name(self.name)
        self.description = normalize_agent_description(self.description)
        self.instructions = normalize_agent_instructions(self.instructions)
        require_enum("Agent.avatar_kind", self.avatar_kind, {"centaeris", "banana"})
        require_enum("Agent.status", self.status, {"active", "deleted"})
        if (
            (self.status == "active")
            != (self.deletedAt is None and self.purgedAt is None)
            or (
                self.purgedAt is not None
                and (self.deletedAt is None or self.purgedAt < self.deletedAt)
            )
        ):
            raise ValueError("Agent deletion state is invalid")
        if not self._state.adding:
            stored_scope = type(self).objects.values_list(
                "workspace_id",
                "owner_id",
                "definition_id",
                "is_business_instance",
            ).get(pk=self.pk)
            if stored_scope != (self.workspace_id, self.owner_id, self.definition_id, self.is_business_instance):
                raise ValueError("Agent ownership is immutable")
            if self.definition_id:
                stored_config = type(self).objects.values_list("name", "description", "instructions", "avatar_kind").get(pk=self.pk)
                if stored_config != (self.name, self.description, self.instructions, self.avatar_kind):
                    raise ValueError("Agent managed configuration is immutable")
        elif self.definition_id:
            version = self.definition.published_version
            if self.definition.workspace_id != self.workspace_id or version is None:
                raise ValueError("Agent definition workspace or publication binding mismatch")
            if (self.name, self.description, self.instructions, self.avatar_kind) != (version.name, version.description, version.instructions, version.avatar_kind):
                raise ValueError("Agent managed configuration must match its published definition")
        return super().save(*args, **kwargs)


class McpBearerCredential(models.Model):
    id = models.CharField(
        primary_key=True,
        max_length=64,
        default=new_mcp_bearer_credential_id,
    )
    plugin_name = models.CharField(max_length=64)
    credential_ref = models.CharField(max_length=64)
    display_name = models.CharField(max_length=160)
    encrypted_secret = models.TextField()
    version = models.PositiveIntegerField(default=1)
    created_by = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.PROTECT,
        related_name="created_mcp_bearer_credentials",
    )
    updated_by = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.PROTECT,
        related_name="updated_mcp_bearer_credentials",
    )
    created_at = models.DateTimeField(auto_now_add=True)
    updated_at = models.DateTimeField(auto_now=True)

    class Meta:
        db_table = "app_core_mcp_bearer_credential"
        constraints = [
            models.UniqueConstraint(
                fields=["plugin_name", "credential_ref"],
                name="mcp_bearer_credential_plugin_ref_unique",
            )
        ]

    def save(self, *args, **kwargs):
        validate_lower_kebab("MCP plugin name", self.plugin_name)
        validate_lower_kebab("MCP bearer credential ref", self.credential_ref)
        validate_display_name(self.display_name)
        if not self.encrypted_secret or self.version <= 0:
            raise ValueError("MCP bearer credential secret and version are required")
        return super().save(*args, **kwargs)


class AgentConnectorCredentialApproval(models.Model):
    """A custodian grants one assistant a reference into the encrypted credential store."""
    id = models.CharField(primary_key=True, max_length=64, default=new_agent_connector_approval_id)
    workspace = models.ForeignKey(Workspace, on_delete=models.PROTECT, related_name="connector_approvals")
    definition = models.ForeignKey(AgentDefinition, on_delete=models.PROTECT, related_name="connector_approvals")
    plugin_name = models.CharField(max_length=64)
    server_id = models.CharField(max_length=64)
    resource_path = models.CharField(max_length=255)
    resource_digest = models.CharField(max_length=71)
    credential = models.ForeignKey(McpBearerCredential, on_delete=models.PROTECT, related_name="assistant_approvals")
    credential_version = models.PositiveIntegerField()
    approved_by = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT, related_name="+")
    approved_at = models.DateTimeField(auto_now_add=True)
    revoked_at = models.DateTimeField(null=True, blank=True)

    class Meta:
        constraints = [models.CheckConstraint(condition=models.Q(credential_version__gt=0), name="connector_approval_version_positive")]

    def save(self, *args, **kwargs):
        from .runtime_contract import require_sha256
        from .plugin_catalog import _require_resource_path
        validate_lower_kebab("connector plugin", self.plugin_name)
        validate_lower_kebab("connector server", self.server_id)
        _require_resource_path(self.resource_path)
        require_sha256("connector resource digest", self.resource_digest)
        if self.workspace_id != self.definition.workspace_id or self.plugin_name != self.credential.plugin_name:
            raise ValueError("Connector approval scope mismatch")
        if self._state.adding:
            if self.approved_by_id != self.credential.created_by_id or not self.approved_by.is_superuser:
                raise ValueError("Connector approval requires the credential custodian")
            if self.credential_version != self.credential.version:
                raise ValueError("Connector approval credential version mismatch")
        else:
            immutable = ("workspace_id", "definition_id", "plugin_name", "server_id", "resource_path",
                         "resource_digest", "credential_id", "credential_version", "approved_by_id")
            stored = type(self).objects.values(*immutable, "revoked_at").get(pk=self.pk)
            if any(stored[field] != getattr(self, field) for field in immutable):
                raise ValueError("Connector approval is immutable")
            if stored["revoked_at"] is not None and self.revoked_at != stored["revoked_at"]:
                raise ValueError("Connector approval revocation is permanent")
        return super().save(*args, **kwargs)


class AgentConnectorBinding(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_agent_connector_binding_id)
    workspace = models.ForeignKey(Workspace, on_delete=models.PROTECT, related_name="connector_bindings")
    definition = models.ForeignKey(AgentDefinition, on_delete=models.PROTECT, related_name="connector_bindings")
    plugin_name = models.CharField(max_length=64)
    server_id = models.CharField(max_length=64)
    approval = models.ForeignKey(AgentConnectorCredentialApproval, on_delete=models.PROTECT, related_name="bindings")
    updated_by = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT, related_name="+")
    updated_at = models.DateTimeField(auto_now=True)

    class Meta:
        constraints = [models.UniqueConstraint(fields=["workspace", "definition", "plugin_name", "server_id"], name="agent_connector_binding_unique")]

    def save(self, *args, **kwargs):
        expected = (self.workspace_id, self.definition_id, self.plugin_name, self.server_id)
        approved = (self.approval.workspace_id, self.approval.definition_id, self.approval.plugin_name, self.approval.server_id)
        if expected != approved or self.definition.workspace_id != self.workspace_id or self.approval.revoked_at is not None:
            raise ValueError("Connector binding approval scope mismatch")
        if not self._state.adding:
            stored = type(self).objects.values_list("workspace_id", "definition_id", "plugin_name", "server_id").get(pk=self.pk)
            if stored != expected:
                raise ValueError("Connector binding scope is immutable")
        return super().save(*args, **kwargs)


class McpCredentialAuditEvent(models.Model):
    id = models.CharField(
        primary_key=True,
        max_length=64,
        default=new_mcp_credential_audit_id,
    )
    credential_id = models.CharField(max_length=64)
    plugin_name = models.CharField(max_length=64)
    credential_ref = models.CharField(max_length=64)
    display_name = models.CharField(max_length=160)
    action = models.CharField(max_length=32)
    actor = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT)
    created_at = models.DateTimeField(auto_now_add=True)

    class Meta:
        db_table = "app_core_mcp_credential_audit_event"

    def save(self, *args, **kwargs):
        require_enum(
            "McpCredentialAuditEvent.action",
            self.action,
            {"created", "rotated", "deleted", "resolved"},
        )
        validate_lower_kebab("MCP plugin name", self.plugin_name)
        validate_lower_kebab("MCP bearer credential ref", self.credential_ref)
        validate_display_name(self.display_name)
        return super().save(*args, **kwargs)


class SessionProject(models.Model):
    id = models.CharField(
        primary_key=True,
        max_length=64,
        default=new_session_project_id,
    )
    workspace = models.ForeignKey(
        Workspace,
        on_delete=models.PROTECT,
        related_name="session_projects",
    )
    owner = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.PROTECT,
        related_name="session_projects",
    )
    agent = models.ForeignKey(
        Agent,
        db_column="agent_id",
        on_delete=models.PROTECT,
        related_name="session_projects",
    )
    name = models.CharField(max_length=100)
    created_at = models.DateTimeField(auto_now_add=True)


class Session(models.Model):
    objects = ResourceQuerySet.as_manager()
    id = models.CharField(primary_key=True, max_length=64, default=new_session_id)
    workspace = models.ForeignKey(
        Workspace, on_delete=models.PROTECT, related_name="sessions"
    )
    owner = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT)
    agent = models.ForeignKey(
        Agent,
        db_column="agent_id",
        on_delete=models.PROTECT,
        related_name="sessions",
    )
    project = models.ForeignKey(
        SessionProject,
        db_column="project_id",
        on_delete=models.PROTECT,
        related_name="sessions",
        null=True,
        blank=True,
    )
    title = models.CharField(max_length=200, default="New chat")
    origin = models.CharField(max_length=32, default="user")
    status = models.CharField(max_length=32, default="active")
    deletedAt = models.DateTimeField(null=True, blank=True)
    deletedBy = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.PROTECT,
        related_name="+",
        null=True,
        blank=True,
    )
    purgedAt = models.DateTimeField(null=True, blank=True)
    isPinned = models.BooleanField(default=False)
    isUnread = models.BooleanField(default=False)
    workspaceGeneration = models.PositiveBigIntegerField(default=0)
    workspaceStorageKey = models.CharField(max_length=1000, blank=True, default="")
    workspaceSnapshotSha256 = models.CharField(max_length=71, blank=True, default="")
    workspaceSnapshotSizeBytes = models.PositiveBigIntegerField(default=0)
    workspaceExpandedSizeBytes = models.PositiveBigIntegerField(default=0)
    workspaceFileCount = models.PositiveIntegerField(default=0)
    workspaceLastAdvancedAgentRun = models.ForeignKey(
        "AgentRun",
        on_delete=models.PROTECT,
        related_name="+",
        null=True,
        blank=True,
    )
    createdAt = models.DateTimeField(auto_now_add=True)
    updatedAt = models.DateTimeField(auto_now=True)

    class Meta:
        constraints = [
            models.CheckConstraint(
                condition=models.Q(status__in=("active", "deleted")),
                name="session_status_valid",
            ),
            models.CheckConstraint(
                condition=(
                    models.Q(
                        status="active",
                        deletedAt__isnull=True,
                        purgedAt__isnull=True,
                    )
                    | (
                        models.Q(status="deleted", deletedAt__isnull=False)
                        & (
                            models.Q(purgedAt__isnull=True)
                            | models.Q(purgedAt__gte=models.F("deletedAt"))
                        )
                    )
                ),
                name="session_deletion_consistency",
            ),
            models.CheckConstraint(
                condition=(
                    models.Q(workspaceGeneration__gte=0)
                    & models.Q(workspaceSnapshotSizeBytes__gte=0)
                    & models.Q(workspaceExpandedSizeBytes__gte=0)
                    & models.Q(workspaceFileCount__gte=0)
                ),
                name="session_workspace_nonnegative",
            ),
            models.CheckConstraint(
                condition=(
                    models.Q(
                        workspaceGeneration=0,
                        workspaceStorageKey="",
                        workspaceSnapshotSha256="",
                        workspaceSnapshotSizeBytes=0,
                        workspaceExpandedSizeBytes=0,
                        workspaceFileCount=0,
                        workspaceLastAdvancedAgentRun__isnull=True,
                    )
                    |
                    models.Q(
                        workspaceGeneration__gt=0,
                        workspaceLastAdvancedAgentRun__isnull=False,
                    )
                ),
                name="session_workspace_generation_consistency",
            ),
            models.CheckConstraint(
                condition=(
                    models.Q(
                        workspaceStorageKey="",
                        workspaceSnapshotSha256="",
                        workspaceSnapshotSizeBytes=0,
                        workspaceExpandedSizeBytes=0,
                        workspaceFileCount=0,
                    )
                    |
                    (
                        ~models.Q(workspaceStorageKey="")
                        & ~models.Q(workspaceSnapshotSha256="")
                        & models.Q(workspaceSnapshotSizeBytes__gt=0)
                        & (
                            models.Q(
                                workspaceFileCount=0,
                                workspaceExpandedSizeBytes=0,
                            )
                            | models.Q(workspaceFileCount__gt=0)
                        )
                    )
                ),
                name="session_workspace_file_count_consistency",
            ),
        ]

    def save(self, *args, **kwargs):
        validate_agent_id(self.agent_id)
        if self._state.adding:
            try:
                agent_scope = Agent.objects.values_list(
                    "workspace_id",
                    "owner_id",
                ).get(pk=self.agent_id)
            except Agent.DoesNotExist as error:
                raise ValueError("Session agent does not exist") from error
            if agent_scope != (self.workspace_id, self.owner_id):
                raise ValueError("Session agent ownership mismatch")
        else:
            stored_scope = type(self).objects.values_list(
                "agent_id",
                "workspace_id",
                "owner_id",
            ).get(pk=self.pk)
            if stored_scope[0] != self.agent_id:
                raise ValueError("Session agent_id is immutable")
            if stored_scope[1:] != (self.workspace_id, self.owner_id):
                raise ValueError("Session ownership is immutable")
        require_enum(
            "Session.origin",
            self.origin,
            {"user", "automation"},
        )
        require_enum("Session.status", self.status, {"active", "deleted"})
        if (
            (self.status == "active")
            != (self.deletedAt is None and self.purgedAt is None)
            or (
                self.purgedAt is not None
                and (self.deletedAt is None or self.purgedAt < self.deletedAt)
            )
        ):
            raise ValueError("Session deletion state is invalid")
        if self.workspaceLastAdvancedAgentRun_id:
            agent_run = self.workspaceLastAdvancedAgentRun
            if agent_run.session_id != self.id or agent_run.workspace_id != self.workspace_id:
                raise ValueError("Session workspace last advanced agent run mismatch")
        return super().save(*args, **kwargs)


class AgentCoordinationSession(models.Model):
    """Explicit private Agent identity, independent of shared definitions."""
    agent = models.OneToOneField(Agent, primary_key=True, on_delete=models.CASCADE,
                                related_name="coordination_binding")
    session = models.OneToOneField(Session, on_delete=models.CASCADE,
                                  related_name="coordination_binding")
    created_at = models.DateTimeField(auto_now_add=True)

    def save(self, *args, **kwargs):
        if (self.session.agent_id != self.agent_id
                or self.session.owner_id != self.agent.owner_id
                or self.session.workspace_id != self.agent.workspace_id):
            raise ValueError("coordination_session_identity_mismatch")
        if not self._state.adding or type(self).objects.filter(pk=self.pk).exists():
            raise ValueError("coordination_session_binding_is_immutable")
        if self.session.events.exists():
            raise ValueError("coordination_session_requires_fresh_history")
        return super().save(*args, **kwargs)


class BusinessAgentBranchQuerySet(models.QuerySet):
    def update(self, **kwargs):
        raise ValueError("business_agent_branch_is_immutable")

    def bulk_update(self, *args, **kwargs):
        raise ValueError("business_agent_branch_is_immutable")

    def delete(self):
        raise ValueError("business_agent_branch_is_immutable")

    def bulk_create(self, *args, **kwargs):
        raise ValueError("business_agent_branch_requires_validated_creation")


class BusinessAgentBranch(models.Model):
    """One application's exact external subject maps permanently to fresh resources."""
    objects = BusinessAgentBranchQuerySet.as_manager()
    id = models.CharField(primary_key=True, max_length=64, default=new_business_agent_branch_id)
    app = models.ForeignKey(BusinessApplication, on_delete=models.PROTECT, related_name="agent_branches")
    root_agent = models.ForeignKey(Agent, on_delete=models.PROTECT, related_name="business_branches")
    business_user_id = models.CharField(max_length=256)
    agent = models.OneToOneField(Agent, on_delete=models.PROTECT, related_name="business_branch")
    session = models.OneToOneField(Session, on_delete=models.PROTECT, related_name="business_branch")
    created_at = models.DateTimeField(auto_now_add=True)

    class Meta:
        constraints = [
            models.UniqueConstraint(fields=["app", "root_agent", "business_user_id"], name="business_branch_subject_unique"),
            models.CheckConstraint(condition=~models.Q(business_user_id=""), name="business_branch_subject_nonempty"),
            models.CheckConstraint(condition=~models.Q(root_agent=models.F("agent")), name="business_branch_agent_distinct"),
        ]

    def save(self, *args, **kwargs):
        from .business_agent_branches import validate_business_user_id
        validate_business_user_id(self.business_user_id)
        if not self._state.adding:
            raise ValueError("business_agent_branch_is_immutable")
        if (self.root_agent_id == self.agent_id or self.root_agent.definition_id != self.agent.definition_id
                or self.root_agent.is_business_instance or not self.agent.is_business_instance or self.root_agent.status != "active"
                or self.agent.status != "active" or self.app.status != "active"
                or self.root_agent.owner_id != self.agent.owner_id
                or self.root_agent.workspace_id != self.agent.workspace_id
                or self.session.agent_id != self.agent_id or self.session.owner_id != self.agent.owner_id
                or self.session.workspace_id != self.agent.workspace_id
                or type(self).objects.filter(agent_id=self.root_agent_id).exists()
                or not AgentCoordinationSession.objects.filter(agent_id=self.agent_id, session_id=self.session_id).exists()):
            raise ValueError("business_agent_branch_binding_rejected")
        kwargs["force_insert"] = True
        return super().save(*args, **kwargs)

    def delete(self, *args, **kwargs):
        raise ValueError("business_agent_branch_is_immutable")


class AgentInput(models.Model):
    """One immutable hosted input fact for pending delivery and retained history."""
    agent = models.ForeignKey(Agent, on_delete=models.CASCADE, related_name="inputs")
    session = models.ForeignKey(Session, on_delete=models.CASCADE, related_name="agent_inputs")
    input_id = models.CharField(max_length=64)
    membership_ref = models.CharField(max_length=64, editable=False)
    acting_app = models.ForeignKey(BusinessApplication, on_delete=models.PROTECT, related_name="agent_inputs", null=True, blank=True)
    app_delegation = models.ForeignKey(UserAppDelegation, on_delete=models.PROTECT, related_name="agent_inputs", null=True, blank=True)
    credential_version = models.PositiveBigIntegerField(null=True, blank=True, editable=False)
    sequence = models.PositiveBigIntegerField()
    accepted_source_sequence = models.PositiveIntegerField()
    body = models.TextField()
    attachments = models.JSONField(default=list)
    created_at_ms = models.BigIntegerField()

    class Meta:
        indexes = [models.Index(fields=["session", "accepted_source_sequence", "sequence"], name="agent_input_history_order")]
        constraints = [
            models.UniqueConstraint(fields=["agent", "input_id"], name="agent_input_identity_unique"),
            models.UniqueConstraint(fields=["agent", "sequence"], name="agent_input_sequence_unique"),
            models.CheckConstraint(condition=models.Q(sequence__gt=0), name="agent_input_sequence_positive"),
            models.CheckConstraint(condition=~models.Q(membership_ref=""), name="agent_input_membership_required"),
            models.CheckConstraint(condition=(models.Q(acting_app__isnull=True, app_delegation__isnull=True, credential_version__isnull=True)
                | models.Q(acting_app__isnull=False, app_delegation__isnull=False, credential_version__isnull=False, credential_version__gt=0)),
                name="agent_input_app_origin_complete"),
        ]

    def save(self, *args, **kwargs):
        from .agent_input_contract import validate_input_body, validate_input_id
        validate_input_id(self.input_id)
        validate_input_body(self.body, allow_empty=bool(self.attachments))
        if not self._state.adding:
            raise ValueError("agent_input_is_immutable")
        if (self.session.agent_id != self.agent_id or self.session.owner_id != self.agent.owner_id
                or self.session.workspace_id != self.agent.workspace_id or not self.membership_ref):
            raise ValueError("agent_input_binding_rejected")
        if self.acting_app_id is None and self.app_delegation_id is None and self.credential_version is None:
            pass
        elif self.acting_app_id is None or self.app_delegation_id is None or type(self.credential_version) is not int:
            raise ValueError("agent_input_application_origin_incomplete")
        else:
            grant = self.app_delegation
            if (self.credential_version <= 0 or self.credential_version != grant.credential_version
                    or grant.app_id != self.acting_app_id or grant.user_id != self.agent.owner_id
                    or grant.workspace_id != self.agent.workspace_id or grant.membership_ref != self.membership_ref
                    or not BusinessAgentBranch.objects.filter(app_id=grant.app_id,
                        agent_id=self.agent_id, session_id=self.session_id).filter(
                            models.Q(root_agent_id=grant.agent_id) if grant.agent_id else
                            models.Q(root_agent__definition_id=grant.definition_id,
                                root_agent__owner_id=grant.user_id, root_agent__workspace_id=grant.workspace_id)).exists()):
                raise ValueError("agent_input_application_origin_binding_rejected")
        kwargs["force_insert"] = True
        return super().save(*args, **kwargs)


class AgentInputQueue(models.Model):
    """One admitted coordinator owns intake until its atomic empty close."""
    agent_run = models.OneToOneField("AgentRun", primary_key=True, on_delete=models.PROTECT,
        related_name="agent_input_queue")
    initial_input = models.ForeignKey(AgentInput, null=True, blank=True, on_delete=models.PROTECT,
        related_name="initial_queues")
    initial_host = models.ForeignKey("AgentWorkConsumeAttempt", null=True, blank=True, on_delete=models.PROTECT,
        related_name="initial_queues")
    authorization_digest = models.CharField(max_length=71)
    accepting = models.BooleanField(default=True)
    closed_reason = models.CharField(max_length=64, blank=True, default="")
    closed_at_ms = models.BigIntegerField(null=True, blank=True)

    class Meta:
        constraints = [models.CheckConstraint(condition=models.Q(initial_input__isnull=True)
            | models.Q(initial_host__isnull=True), name="agent_input_queue_one_initial_owner")]

    def save(self, *args, **kwargs):
        if not self._state.adding:
            raise ValueError("agent_input_queue_identity_is_immutable")
        run = self.agent_run
        if self.authorization_digest != run.authorization.digest:
            raise ValueError("agent_input_queue_authorization_rejected")
        if self.initial_input_id is not None and self.initial_host_id is not None:
            raise ValueError("agent_input_queue_initial_owner_conflict")
        if self.initial_input_id is not None and (
            self.initial_input.session_id != run.session_id
            or self.initial_input.membership_ref != run.membership_ref
            or self.initial_input.body != run.prompt):
            raise ValueError("agent_input_queue_initial_binding_rejected")
        kwargs["force_insert"] = True
        return super().save(*args, **kwargs)


class AgentInputDelivery(models.Model):
    """Stable input-to-Run ownership; Runtime owns only claim/ACK bookkeeping."""
    input = models.OneToOneField(AgentInput, primary_key=True, on_delete=models.CASCADE,
        related_name="delivery")
    queue = models.ForeignKey(AgentInputQueue, on_delete=models.PROTECT, related_name="deliveries")
    claim_token = models.TextField(null=True, blank=True)
    claim_lease_owner = models.TextField(null=True, blank=True)
    acknowledged_at_ms = models.BigIntegerField(null=True, blank=True)

    def save(self, *args, **kwargs):
        if not self._state.adding:
            raise ValueError("agent_input_delivery_identity_is_immutable")
        run = self.queue.agent_run
        if self.input.session_id != run.session_id or self.input.membership_ref != run.membership_ref:
            raise ValueError("agent_input_delivery_binding_rejected")
        kwargs["force_insert"] = True
        return super().save(*args, **kwargs)


class AgentRun(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_agent_run_id)
    turn_id = models.CharField(max_length=64, default=new_turn_id, unique=True)
    membership_ref = models.CharField(max_length=64, editable=False)
    workspace = models.ForeignKey(Workspace, on_delete=models.PROTECT)
    session = models.ForeignKey(
        Session, on_delete=models.PROTECT, related_name="agent_runs"
    )
    user = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT)
    acting_app = models.ForeignKey(BusinessApplication, on_delete=models.PROTECT, related_name="agent_runs", null=True, blank=True)
    app_delegation = models.ForeignKey(UserAppDelegation, on_delete=models.PROTECT, related_name="agent_runs", null=True, blank=True)
    modelConfig = models.ForeignKey(ModelConfig, on_delete=models.PROTECT)
    thinkingMode = models.CharField(max_length=64, blank=True, default="")
    prompt = models.TextField()
    agent_instructions = models.TextField(blank=True, default="")
    definition_version = models.ForeignKey(
        AgentDefinitionVersion, on_delete=models.PROTECT, related_name="agent_runs", null=True, blank=True,
    )
    tailPolicy = models.CharField(max_length=32, default="append")
    rewriteTargetMessageId = models.CharField(max_length=160, blank=True, default="")
    rewriteExpectedTailMessageId = models.CharField(max_length=160, blank=True, default="")
    status = models.CharField(max_length=32, default="queued")
    transitionReason = models.CharField(max_length=160, default="agent_run_created")
    startedAt = models.DateTimeField(null=True, blank=True)
    completedAt = models.DateTimeField(null=True, blank=True)
    # Runtime-owned receipt; cancellation requests alone are not terminal facts.
    preAdmissionCancelledAt = models.DateTimeField(null=True, blank=True, editable=False)
    createdAt = models.DateTimeField(auto_now_add=True)
    updatedAt = models.DateTimeField(auto_now=True)

    class Meta:
        indexes = [
            models.Index(fields=["workspace", "createdAt", "id"],
                         condition=models.Q(status="queued", startedAt__isnull=True),
                         name="agent_run_initial_queue"),
        ]
        constraints = [
            models.CheckConstraint(
                condition=~models.Q(membership_ref=""),
                name="agent_run_membership_ref_required",
            ),
            models.CheckConstraint(condition=(models.Q(acting_app__isnull=True, app_delegation__isnull=True)
                | models.Q(acting_app__isnull=False, app_delegation__isnull=False)), name="agent_run_app_origin_valid"),
        ]

    def save(self, *args, **kwargs):
        self.agent_instructions = normalize_agent_instructions(self.agent_instructions)
        if (self.acting_app_id is None) != (self.app_delegation_id is None):
            raise ValueError("AgentRun application origin is incomplete")
        if self.app_delegation_id:
            delegation = self.app_delegation
            if (delegation.agent_id is not None or delegation.app_id != self.acting_app_id or delegation.user_id != self.user_id
                    or delegation.workspace_id != self.workspace_id
                    or delegation.definition_id != self.session.agent.definition_id):
                raise ValueError("AgentRun application origin binding mismatch")
        if not self._state.adding:
            if type(self).objects.values_list("acting_app_id", "app_delegation_id").get(pk=self.pk) != (self.acting_app_id, self.app_delegation_id):
                raise ValueError("AgentRun application origin is immutable")
        if self._state.adding:
            definition_id = self.session.agent.definition_id
            if self.definition_version_id:
                if self.definition_version.definition_id != definition_id or self.agent_instructions != self.definition_version.instructions:
                    raise ValueError("AgentRun definition version binding mismatch")
            elif definition_id:
                raise ValueError("Managed AgentRun requires a published definition version")
        else:
            stored_snapshot = type(self).objects.values_list("definition_version_id", "agent_instructions").get(pk=self.pk)
            if stored_snapshot != (self.definition_version_id, self.agent_instructions):
                raise ValueError("AgentRun configuration snapshot is immutable")
        if self.thinkingMode:
            validate_thinking_mode(self.thinkingMode)
        require_enum(
            "AgentRun.status",
            self.status,
            {"queued", "running", "completed", "failed", "cancelled"},
        )
        require_enum("AgentRun.tailPolicy", self.tailPolicy, {"append", "rewriteLastUser"})
        if self.tailPolicy == "append":
            if self.rewriteTargetMessageId or self.rewriteExpectedTailMessageId:
                raise ValueError("append AgentRun must not carry rewrite identities")
        elif not self.rewriteTargetMessageId or not self.rewriteExpectedTailMessageId:
            raise ValueError("rewrite AgentRun requires target and expected tail message ids")
        if (
            self.workspace_id != self.session.workspace_id
            or self.user_id != self.session.owner_id
        ):
            raise ValueError("AgentRun workspace/session/user binding mismatch")
        if self._state.adding:
            membership = WorkspaceMembership.objects.filter(
                workspace_id=self.workspace_id,
                workspace__status="active",
                user_id=self.user_id,
                role__in=WORKSPACE_ROLES,
            ).only("id").first()
            if membership is None:
                raise ValueError("AgentRun requires active WorkspaceMembership")
            if self.membership_ref and self.membership_ref != membership.id:
                raise ValueError("AgentRun membership binding mismatch")
            self.membership_ref = membership.id
        elif not self.membership_ref:
            raise ValueError("AgentRun membership binding is missing")
        if not self.turn_id.strip() or self.turn_id == self.id:
            raise ValueError("AgentRun initial Turn identity is invalid")
        return super().save(*args, **kwargs)


class HostedOperationReceipt(models.Model):
    """Minimal accepted command identity, retained even when its resources disappear."""

    acting_app = models.ForeignKey(BusinessApplication, on_delete=models.PROTECT, related_name="operation_receipts", null=True, blank=True)
    app_delegation = models.ForeignKey(UserAppDelegation, on_delete=models.PROTECT, related_name="operation_receipts", null=True, blank=True)

    user = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT)
    workspace = models.ForeignKey(Workspace, on_delete=models.PROTECT)
    command = models.CharField(max_length=32)
    operationId = models.CharField(max_length=128)
    requestDigest = models.CharField(max_length=64)
    # Deliberately not cascading FKs: resource erasure must not enable resubmission.
    sessionId = models.CharField(max_length=64)
    agentRunId = models.CharField(max_length=64, null=True)
    turnId = models.CharField(max_length=64, null=True)
    acceptedAt = models.DateTimeField(auto_now_add=True)

    class Meta:
        constraints = [
            models.UniqueConstraint(
                fields=["user", "workspace", "command", "operationId"],
                name="hosted_operation_scope_unique",
            ),
            models.CheckConstraint(
                condition=(
                    models.Q(command="createSession", agentRunId__isnull=True, turnId__isnull=True)
                    | models.Q(command__in=["submitMessage", "consumeWorkReturn"], agentRunId__isnull=False, turnId__isnull=False)
                    | models.Q(command="consumeWorkReturn", agentRunId__isnull=True, turnId__isnull=True)
                ),
                name="hosted_operation_result_valid",
            ),
        ]


class AgentWorkSession(models.Model):
    """Immutable dispatch origin and independent admission, without task state."""
    session = models.OneToOneField(Session, primary_key=True, on_delete=models.CASCADE, related_name="work_binding")
    coordination_session = models.ForeignKey(Session, on_delete=models.PROTECT, related_name="dispatched_work")
    source_run = models.ForeignKey(AgentRun, on_delete=models.PROTECT, related_name="dispatched_work")
    source_turn_id = models.CharField(max_length=160)
    source_call_id = models.CharField(max_length=160)
    # Retain the origin identity through source event erasure, like operation receipts.
    source_event_id = models.CharField(max_length=160, unique=True)
    operation = models.OneToOneField(HostedOperationReceipt, on_delete=models.PROTECT, related_name="work_binding")
    created_at = models.DateTimeField(auto_now_add=True)

    class Meta:
        constraints = [models.UniqueConstraint(fields=["source_run", "source_turn_id", "source_call_id"],
                                              name="agent_work_source_unique")]

    def save(self, *args, **kwargs):
        if (self.session_id == self.coordination_session_id
                or self.session.agent_id != self.coordination_session.agent_id
                or self.session.owner_id != self.coordination_session.owner_id
                or self.session.workspace_id != self.coordination_session.workspace_id
                or self.source_run.session_id != self.coordination_session_id
                or self.source_run.user_id != self.session.owner_id
                or self.source_run.workspace_id != self.session.workspace_id
                or self.operation.sessionId != self.session_id
                or not AgentRun.objects.filter(id=self.operation.agentRunId, session_id=self.session_id,
                    user_id=self.session.owner_id, workspace_id=self.session.workspace_id,
                    turn_id=self.operation.turnId).exists()
                or self.operation.user_id != self.session.owner_id
                or self.operation.workspace_id != self.session.workspace_id):
            raise ValueError("agent_work_binding_identity_mismatch")
        if not self._state.adding or type(self).objects.filter(pk=self.pk).exists():
            raise ValueError("agent_work_binding_is_immutable")
        return super().save(*args, **kwargs)


class TranscriptOutputCapture(models.Model):
    """Hosted, immutable UTF-8 output associated with a committed Core event."""

    event = models.OneToOneField("SessionEvent", primary_key=True, on_delete=models.CASCADE)
    executionId = models.CharField(max_length=160)
    sha256 = models.CharField(max_length=71)
    byteLength = models.PositiveBigIntegerField()
    chunkSize = models.PositiveIntegerField()
    createdAt = models.DateTimeField(auto_now_add=True)
    purgedAt = models.DateTimeField(null=True)


class TranscriptOutputChunk(models.Model):
    capture = models.ForeignKey(TranscriptOutputCapture, on_delete=models.CASCADE, related_name="chunks")
    index = models.PositiveIntegerField()
    data = models.BinaryField()
    sha256 = models.CharField(max_length=71)

    class Meta:
        constraints = [models.UniqueConstraint(fields=["capture", "index"], name="unique_transcript_output_chunk")]


class AgentWorkReturn(models.Model):
    """Immutable delivered fact; it does not record consumption or task success."""
    id = models.CharField(primary_key=True, max_length=96)
    work = models.ForeignKey(AgentWorkSession, on_delete=models.PROTECT, related_name="returns")
    child_run = models.ForeignKey(AgentRun, on_delete=models.PROTECT, related_name="work_returns")
    fact_kind = models.CharField(max_length=32)
    fact_ref = models.CharField(max_length=256)
    payload = models.JSONField()
    delivered_at = models.DateTimeField(auto_now_add=True)

    class Meta:
        constraints = [models.UniqueConstraint(fields=["work", "child_run", "fact_kind", "fact_ref"],
            name="agent_work_return_fact_unique")]

    def save(self, *args, **kwargs):
        require_enum("AgentWorkReturn.fact_kind", self.fact_kind,
            {"sessionTerminal", "preAdmissionCancellation", "lifecycleFailure"})
        if self.work.operation.agentRunId != self.child_run_id or self.child_run.session_id != self.work_id:
            raise ValueError("agent_work_return_identity_mismatch")
        if not self._state.adding:
            raise ValueError("agent_work_return_is_immutable")
        # An INSERT collision lets get_or_create reread the committed winner.
        # Never let a fresh instance silently UPDATE an existing primary key.
        kwargs["force_insert"] = True
        return super().save(*args, **kwargs)


class AgentWorkConsumeAttempt(models.Model):
    """The one explicit first admission for a retained notice, without handling state."""
    id = models.CharField(primary_key=True, max_length=64, default=new_agent_run_id)
    notice = models.OneToOneField(AgentWorkReturn, on_delete=models.PROTECT, related_name="first_consume")
    coordinator_run = models.ForeignKey(AgentRun, null=True, blank=True, on_delete=models.PROTECT, related_name="work_consumes")
    operation = models.OneToOneField(HostedOperationReceipt, on_delete=models.PROTECT, related_name="work_consume")
    input_binding = models.JSONField()
    delivery_sequence = models.PositiveBigIntegerField()
    created_at_ms = models.BigIntegerField(default=0)
    claim_token = models.TextField(null=True, blank=True)
    claim_lease_owner = models.TextField(null=True, blank=True)
    acknowledged_at_ms = models.BigIntegerField(null=True, blank=True)

    def save(self, *args, **kwargs):
        if not self._state.adding:
            raise ValueError("agent_work_consume_is_immutable")
        if (self.operation.command != "consumeWorkReturn"
                or self.operation.agentRunId != self.coordinator_run_id
                or self.operation.turnId != (self.coordinator_run.turn_id if self.coordinator_run_id else None)
                or self.operation.sessionId != self.notice.work.coordination_session_id
                or (self.coordinator_run_id and self.coordinator_run.session_id != self.operation.sessionId)):
            raise ValueError("agent_work_consume_binding_rejected")
        kwargs["force_insert"] = True
        return super().save(*args, **kwargs)


class SessionEvent(models.Model):
    eventId = models.CharField(primary_key=True, max_length=160)
    workspace = models.ForeignKey(Workspace, on_delete=models.PROTECT)
    session = models.ForeignKey(
        Session, on_delete=models.PROTECT, related_name="events"
    )
    agent_run = models.ForeignKey(AgentRun, on_delete=models.PROTECT, related_name="events")
    sequence = models.PositiveIntegerField()
    agent_run_sequence = models.PositiveIntegerField(null=True, blank=True)
    session_level = models.BooleanField(default=False, db_default=False)
    projects_to_agent_run_stream = models.BooleanField()
    payload = SessionPayloadField()
    createdAtMs = models.BigIntegerField()
    insertedAt = models.DateTimeField(auto_now_add=True)

    class Meta:
        indexes = [models.Index(fields=["insertedAt", "eventId"], name="agent_work_recovery_scan",
            condition=models.Q(session_level=False, projects_to_agent_run_stream=True,
                payload__type="tool_result", payload__payload__toolName="dispatch_work",
                payload__payload__resultState="successWithOutput"))]
        constraints = [
            models.UniqueConstraint(
                fields=["session", "sequence"],
                name="unique_session_event_session_sequence",
            ),
            models.UniqueConstraint(
                fields=["agent_run", "agent_run_sequence"],
                name="unique_session_event_agent_run_sequence",
            ),
            # Session-level recovery facts carry no AgentRun sequence and never
            # enter the per-run stream; ordinary run facts must carry a positive
            # run sequence.
            models.CheckConstraint(
                condition=(
                    models.Q(
                        session_level=True,
                        agent_run_sequence__isnull=True,
                        projects_to_agent_run_stream=False,
                    )
                    | models.Q(
                        session_level=False,
                        agent_run_sequence__isnull=False,
                        agent_run_sequence__gt=0,
                    )
                ),
                name="session_event_level_shape",
            ),
            # session_level is derived from the canonical event type. Only
            # tool_call_closure is session-level today; widening this requires an
            # explicit mapping change here and in the runtime writer.
            models.CheckConstraint(
                condition=(
                    models.Q(session_level=True, payload__type="tool_call_closure")
                    | models.Q(session_level=False)
                    & ~models.Q(payload__type="tool_call_closure")
                ),
                name="session_event_session_level_matches_type",
            ),
        ]

    def save(self, *args, **kwargs):
        if not self._state.adding:
            raise ValueError("SessionEvent is append-only")
        return super().save(*args, **kwargs)


class SessionCitationProjection(models.Model):
    citationId = models.CharField(primary_key=True, max_length=160)
    workspace = models.ForeignKey(Workspace, on_delete=models.CASCADE)
    session = models.ForeignKey(
        Session,
        on_delete=models.CASCADE,
        related_name="citationProjections",
    )
    agent_run = models.ForeignKey(
        AgentRun,
        on_delete=models.CASCADE,
        related_name="citationProjections",
    )
    sequence = models.PositiveIntegerField()
    inputRef = models.CharField(max_length=1024)
    ownerRef = models.CharField(max_length=160)
    ownerKind = models.CharField(max_length=32)
    displayName = models.CharField(max_length=255)
    evidenceKind = models.CharField(max_length=32)
    ownerSha256 = models.CharField(max_length=71)
    ownerGeneration = models.PositiveBigIntegerField(default=1)
    representationId = models.CharField(max_length=96, blank=True, default="")
    specDigest = models.CharField(max_length=71, blank=True, default="")
    evidenceSha256 = models.CharField(max_length=71, blank=True, default="")
    sourceToolName = models.CharField(max_length=64, default="read")
    sourceToolCallId = models.CharField(max_length=160)
    locator = models.JSONField()


class AgentRunAuthorization(models.Model):
    id = models.CharField(
        primary_key=True, max_length=64, default=new_agent_run_authorization_id
    )
    agent_run = models.OneToOneField(
        AgentRun, on_delete=models.PROTECT, related_name="authorization"
    )
    payload = models.JSONField()
    digest = models.CharField(max_length=71)
    signature = models.CharField(max_length=76)
    createdAt = models.DateTimeField(auto_now_add=True)

    def save(self, *args, **kwargs):
        if not self._state.adding:
            raise ValueError("AgentRunAuthorization is immutable")
        from django.conf import settings
        from .runtime_contract import (
            authorization_digest,
            session_workspace_for_session,
            validate_agent_run_authorization_payload,
            verify_agent_run_authorization_signature,
        )

        validate_agent_run_authorization_payload(self.payload)
        if authorization_digest(self.payload) != self.digest:
            raise ValueError("AgentRunAuthorization digest mismatch")
        verify_agent_run_authorization_signature(
            self.payload, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY, self.signature
        )
        agent_run = self.agent_run
        if (
            self.payload["id"] != self.id
            or self.payload["agentRunId"] != agent_run.id
            or self.payload["workspaceId"] != agent_run.workspace_id
            or self.payload["userId"] != str(agent_run.user_id)
            or self.payload["agentId"] != agent_run.session.agent_id
            or self.payload["sessionId"] != agent_run.session_id
            or self.payload["modelConfigRef"] != agent_run.modelConfig_id
            or self.payload["thinkingMode"] != (agent_run.thinkingMode or None)
            or self.payload["sessionWorkspace"]
            != session_workspace_for_session(agent_run.session)
        ):
            raise ValueError("AgentRunAuthorization binding mismatch")
        return super().save(*args, **kwargs)


class Source(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_source_id)
    workspace = models.ForeignKey(
        Workspace, on_delete=models.PROTECT, related_name="sources"
    )
    sourceType = models.CharField(max_length=32)
    name = models.CharField(max_length=255)
    status = models.CharField(max_length=32, default="processing")
    deletedAt = models.DateTimeField(null=True, blank=True)
    deletedBy = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.PROTECT,
        related_name="+",
        null=True,
        blank=True,
    )
    purgedAt = models.DateTimeField(null=True, blank=True)
    deletedFromStatus = models.CharField(max_length=32, blank=True, default="")
    createdBy = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT)
    lastSyncedAt = models.DateTimeField(null=True, blank=True)
    lastIndexedAt = models.DateTimeField(null=True, blank=True)
    failureReason = models.TextField(blank=True, default="")
    createdAt = models.DateTimeField(auto_now_add=True)
    updatedAt = models.DateTimeField(auto_now=True)

    class Meta:
        constraints = [
            models.CheckConstraint(
                condition=models.Q(
                    status__in=("processing", "ready", "failed", "deleted")
                ),
                name="source_status_valid",
            ),
            models.CheckConstraint(
                condition=(
                    models.Q(
                        status__in=("processing", "ready", "failed"),
                        deletedAt__isnull=True,
                        purgedAt__isnull=True,
                        deletedFromStatus="",
                    )
                    | models.Q(
                        status="deleted",
                        deletedAt__isnull=False,
                        deletedFromStatus__in=("processing", "ready", "failed"),
                    )
                    & (
                        models.Q(purgedAt__isnull=True)
                        | models.Q(purgedAt__gte=models.F("deletedAt"))
                    )
                ),
                name="source_lifecycle_shape_valid",
            ),
        ]

    def save(self, *args, **kwargs):
        require_enum("Source.sourceType", self.sourceType, {"uploadedFile", "fileTree"})
        require_enum(
            "Source.status",
            self.status,
            {"processing", "ready", "failed", "deleted"},
        )
        if self.status in {"processing", "ready", "failed"}:
            lifecycle_valid = (
                self.deletedAt is None
                and self.purgedAt is None
                and not self.deletedFromStatus
            )
        else:
            lifecycle_valid = (
                self.deletedAt is not None
                and self.deletedFromStatus in {"processing", "ready", "failed"}
                and (self.purgedAt is None or self.purgedAt >= self.deletedAt)
            )
        if not lifecycle_valid:
            raise ValueError("Source lifecycle shape is invalid")
        require_failure_reason(self.status, self.failureReason)
        return super().save(*args, **kwargs)


class SourceObject(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_source_object_id)
    workspace = models.ForeignKey(Workspace, on_delete=models.PROTECT)
    source = models.ForeignKey(
        Source, on_delete=models.PROTECT, related_name="sourceObjects"
    )
    objectType = models.CharField(max_length=16)
    displayPath = models.CharField(max_length=1000)
    displayName = models.CharField(max_length=255)
    contentType = models.CharField(max_length=160, blank=True, default="")
    sizeBytes = models.BigIntegerField(null=True, blank=True)
    sha256 = models.CharField(max_length=71, blank=True, default="")
    storageKey = models.CharField(max_length=1000, blank=True, default="")
    sourceVersion = models.CharField(max_length=160)
    contentGeneration = models.PositiveBigIntegerField(default=1)
    status = models.CharField(max_length=32, default="processing")
    failureReason = models.TextField(blank=True, default="")
    deletedAt = models.DateTimeField(null=True, blank=True)
    deletionGeneration = models.PositiveIntegerField(default=0)
    createdAt = models.DateTimeField(auto_now_add=True)
    updatedAt = models.DateTimeField(auto_now=True)

    class Meta:
        constraints = [
            models.UniqueConstraint(
                fields=["source", "displayPath"], name="unique_source_object_path"
            )
        ]

    def save(self, *args, **kwargs):
        require_enum("SourceObject.objectType", self.objectType, {"file", "directory"})
        require_enum(
            "SourceObject.status",
            self.status,
            {"processing", "ready", "failed", "deleted"},
        )
        require_failure_reason(self.status, self.failureReason)
        if self.workspace_id != self.source.workspace_id:
            raise ValueError("SourceObject workspace/source binding mismatch")
        return super().save(*args, **kwargs)


class SourceGrant(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_source_grant_id)
    workspace = models.ForeignKey(Workspace, on_delete=models.PROTECT)
    source = models.ForeignKey(Source, on_delete=models.PROTECT, related_name="grants")
    workspaceGroup = models.ForeignKey(WorkspaceGroup, on_delete=models.PROTECT)
    accessLevel = models.CharField(max_length=16, default="read")
    createdBy = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT)
    createdAt = models.DateTimeField(auto_now_add=True)

    class Meta:
        constraints = [
            models.UniqueConstraint(
                fields=["source", "workspaceGroup"],
                name="unique_source_workspace_group_grant",
            ),
            models.CheckConstraint(
                condition=models.Q(accessLevel__in=("read", "write", "control")),
                name="source_grant_access_level_valid",
            ),
        ]

    def save(self, *args, **kwargs):
        require_enum("SourceGrant.accessLevel", self.accessLevel, SOURCE_ACCESS_LEVELS)
        if self.workspace_id != self.source.workspace_id:
            raise ValueError("SourceGrant workspace/source binding mismatch")
        if self.workspace_id != self.workspaceGroup.workspace_id:
            raise ValueError("SourceGrant workspace/group binding mismatch")
        return super().save(*args, **kwargs)


class UserLibraryObject(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_library_object_id)
    owner = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.PROTECT,
        related_name="libraryObjects",
    )
    displayName = models.CharField(max_length=255)
    objectKind = models.CharField(max_length=32)
    contentType = models.CharField(max_length=160, blank=True, default="")
    sizeBytes = models.BigIntegerField(null=True, blank=True)
    sha256 = models.CharField(max_length=71, blank=True, default="")
    storageKey = models.CharField(max_length=1000, blank=True, default="")
    parentFolder = models.ForeignKey(
        "self",
        on_delete=models.PROTECT,
        null=True,
        blank=True,
        related_name="children",
    )
    status = models.CharField(max_length=32, default="processing")
    failureReason = models.TextField(blank=True, default="")
    deletedAt = models.DateTimeField(null=True, blank=True)
    deletedBy = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.PROTECT,
        related_name="+",
        null=True,
        blank=True,
    )
    purgedAt = models.DateTimeField(null=True, blank=True)
    deletedFromStatus = models.CharField(max_length=32, blank=True, default="")
    deletionGeneration = models.PositiveIntegerField(default=0)
    contentGeneration = models.PositiveBigIntegerField(default=0)
    createdAt = models.DateTimeField(auto_now_add=True)
    updatedAt = models.DateTimeField(auto_now=True)

    class Meta:
        constraints = [
            models.CheckConstraint(
                condition=(
                    models.Q(
                        status="deleted",
                        deletedAt__isnull=False,
                        deletedFromStatus__in=("processing", "ready", "failed"),
                    )
                    & (
                        models.Q(purgedAt__isnull=True)
                        | models.Q(purgedAt__gte=models.F("deletedAt"))
                    )
                    | (
                        ~models.Q(status="deleted")
                        & models.Q(
                            deletedAt__isnull=True,
                            purgedAt__isnull=True,
                            deletedFromStatus="",
                        )
                    )
                ),
                name="library_object_deletion_consistency",
            )
        ]

    def save(self, *args, **kwargs):
        require_enum(
            "UserLibraryObject.objectKind",
            self.objectKind,
            {"file", "image", "folder", "note", "savedArtifact"},
        )
        require_enum(
            "UserLibraryObject.status",
            self.status,
            {"processing", "ready", "failed", "deleted"},
        )
        require_failure_reason(self.status, self.failureReason)
        if self.status == "deleted":
            require_enum(
                "UserLibraryObject.deletedFromStatus",
                self.deletedFromStatus,
                {"processing", "ready", "failed"},
            )
            if self.deletedAt is None:
                raise ValueError("deleted UserLibraryObject requires deletedAt")
            if self.purgedAt is not None and self.purgedAt < self.deletedAt:
                raise ValueError("purged UserLibraryObject predates deletion")
        elif self.deletedAt is not None or self.purgedAt is not None or self.deletedFromStatus:
            raise ValueError("active UserLibraryObject has deletion metadata")
        if self.parentFolder_id and not type(self).objects.filter(
            id=self.parentFolder_id,
            owner_id=self.owner_id,
            objectKind="folder",
        ).exists():
            raise ValueError("UserLibraryObject parent ownership mismatch")
        if not self._state.adding:
            stored_owner_id = type(self).objects.values_list("owner_id", flat=True).get(
                pk=self.pk
            )
            if stored_owner_id != self.owner_id:
                raise ValueError("UserLibraryObject ownership is immutable")
        return super().save(*args, **kwargs)


class UserLibraryLink(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_library_link_id)
    libraryObject = models.ForeignKey(
        UserLibraryObject,
        on_delete=models.PROTECT,
        related_name="provenanceLinks",
    )
    sourceKind = models.CharField(max_length=32)
    sourceRefId = models.CharField(max_length=160, blank=True, default="")
    createdAt = models.DateTimeField(auto_now_add=True)

    def save(self, *args, **kwargs):
        require_enum(
            "UserLibraryLink.sourceKind",
            self.sourceKind,
            {"upload", "artifact", "manual"},
        )
        if not self._state.adding:
            raise ValueError("UserLibraryLink is immutable")
        return super().save(*args, **kwargs)


class Artifact(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_artifact_id)
    workspace = models.ForeignKey(Workspace, on_delete=models.PROTECT)
    agent_run = models.ForeignKey(
        AgentRun, on_delete=models.PROTECT, related_name="artifacts"
    )
    session = models.ForeignKey(Session, on_delete=models.PROTECT)
    createdBy = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT)
    displayName = models.CharField(max_length=255)
    safeFilename = models.CharField(max_length=255)
    contentType = models.CharField(max_length=160, default="application/octet-stream")
    sizeBytes = models.BigIntegerField()
    sha256 = models.CharField(max_length=71)
    contentGeneration = models.PositiveBigIntegerField(default=1)
    storageKey = models.CharField(max_length=1000)
    status = models.CharField(max_length=32, default="staging")
    publishedAt = models.DateTimeField(null=True, blank=True)
    failureReason = models.TextField(blank=True, default="")
    deletedAt = models.DateTimeField(null=True, blank=True)
    deletionGeneration = models.PositiveIntegerField(default=0)
    createdAt = models.DateTimeField(auto_now_add=True)

    def save(self, *args, **kwargs):
        require_enum(
            "Artifact.status",
            self.status,
            {"staging", "published", "failed", "deleted"},
        )
        require_failure_reason(self.status, self.failureReason)
        if self.status == "published" and self.publishedAt is None:
            raise ValueError("published Artifact requires publishedAt")
        if (
            self.workspace_id != self.session.workspace_id
            or self.workspace_id != self.agent_run.workspace_id
            or self.session_id != self.agent_run.session_id
            or self.createdBy_id != self.agent_run.user_id
        ):
            raise ValueError("Artifact AgentRun/Session/Workspace/user binding mismatch")
        return super().save(*args, **kwargs)


class ArtifactPublication(models.Model):
    publicationId = models.CharField(primary_key=True, max_length=96)
    agent_run = models.ForeignKey(
        AgentRun, on_delete=models.PROTECT, related_name="artifactPublications"
    )
    authorizationDigest = models.CharField(max_length=71)
    toolCallId = models.CharField(max_length=160)
    filename = models.CharField(max_length=255)
    sizeBytes = models.BigIntegerField()
    sha256 = models.CharField(max_length=71)
    status = models.CharField(max_length=32, default="staging")
    artifact = models.ForeignKey(
        Artifact,
        on_delete=models.PROTECT,
        null=True,
        blank=True,
        related_name="publications",
    )
    publishedAt = models.DateTimeField(null=True, blank=True)
    createdAt = models.DateTimeField(auto_now_add=True)

    class Meta:
        constraints = [
            models.UniqueConstraint(
                fields=["agent_run", "toolCallId"],
                name="unique_artifact_publication_agent_run_tool_call",
            )
        ]

    def save(self, *args, **kwargs):
        require_enum("ArtifactPublication.status", self.status, {"staging", "published"})
        if self.status == "published" and (
            self.artifact_id is None or self.publishedAt is None
        ):
            raise ValueError(
                "published ArtifactPublication requires artifact and publishedAt"
            )
        if self.artifact_id is not None and self.artifact.agent_run_id != self.agent_run_id:
            raise ValueError("ArtifactPublication AgentRun binding mismatch")
        return super().save(*args, **kwargs)


class DerivedResource(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_derived_resource_id)
    ownerKind = models.CharField(max_length=32)
    ownerId = models.CharField(max_length=64)
    ownerContentGeneration = models.PositiveBigIntegerField()
    deletionGeneration = models.PositiveIntegerField(default=0)
    resourceKind = models.CharField(max_length=32)
    resourceKey = models.CharField(max_length=1000)
    state = models.CharField(max_length=32, default="active")
    tombstonedAt = models.DateTimeField(null=True, blank=True)
    leaseOwner = models.CharField(max_length=96, blank=True, default="")
    leaseExpiresAt = models.DateTimeField(null=True, blank=True)
    cleanupAttempts = models.PositiveIntegerField(default=0)
    nextCleanupAt = models.DateTimeField(null=True, blank=True)
    quarantinedAt = models.DateTimeField(null=True, blank=True)
    lastFailure = models.TextField(blank=True, default="")
    cleanedAt = models.DateTimeField(null=True, blank=True)
    createdAt = models.DateTimeField(auto_now_add=True)
    updatedAt = models.DateTimeField(auto_now=True)

    class Meta:
        constraints = [
            models.UniqueConstraint(
                fields=[
                    "ownerKind",
                    "ownerId",
                    "ownerContentGeneration",
                    "deletionGeneration",
                    "resourceKind",
                    "resourceKey",
                ],
                name="unique_derived_resource_owner_generation",
            )
        ]
        indexes = [
            models.Index(
                fields=["state", "tombstonedAt"],
                name="derived_resource_gc_due",
            )
        ]

    def save(self, *args, **kwargs):
        require_enum(
            "DerivedResource.ownerKind",
            self.ownerKind,
            {"sourceObject", "userLibraryObject", "artifact"},
        )
        require_enum(
            "DerivedResource.resourceKind",
            self.resourceKind,
            {"storageObject"},
        )
        require_enum(
            "DerivedResource.state",
            self.state,
            {"active", "pending", "cleaning", "cleaned", "failed", "quarantined"},
        )
        if self.state == "active" and self.tombstonedAt is not None:
            raise ValueError("active DerivedResource cannot be tombstoned")
        if self.state != "active" and self.tombstonedAt is None:
            raise ValueError("tombstoned DerivedResource requires tombstonedAt")
        if self.state == "cleaned" and self.cleanedAt is None:
            raise ValueError("cleaned DerivedResource requires cleanedAt")
        if self.state == "quarantined" and self.quarantinedAt is None:
            raise ValueError("quarantined DerivedResource requires quarantinedAt")
        return super().save(*args, **kwargs)


class ProcessingSpecification(models.Model):
    specDigest = models.CharField(primary_key=True, max_length=71)
    payload = models.JSONField()
    createdAt = models.DateTimeField(auto_now_add=True)


class DerivedRepresentation(models.Model):
    representationId = models.CharField(primary_key=True, max_length=96)
    ownerKind = models.CharField(max_length=32)
    ownerId = models.CharField(max_length=64)
    ownerContentGeneration = models.PositiveBigIntegerField()
    ownerSha256 = models.CharField(max_length=71)
    processingSpecification = models.ForeignKey(
        ProcessingSpecification,
        db_column="specDigest",
        on_delete=models.PROTECT,
        related_name="representations",
    )
    pageCount = models.PositiveIntegerField()
    canonicalTextKey = models.CharField(max_length=1000)
    canonicalTextSizeBytes = models.PositiveBigIntegerField()
    canonicalTextSha256 = models.CharField(max_length=71)
    previewPdfKey = models.CharField(max_length=1000, blank=True, default="")
    previewPdfSizeBytes = models.PositiveBigIntegerField(default=0)
    previewPdfSha256 = models.CharField(max_length=71, blank=True, default="")
    workbookPreviewKey = models.CharField(max_length=1000, blank=True, default="")
    workbookPreviewSizeBytes = models.PositiveBigIntegerField(default=0)
    workbookPreviewSha256 = models.CharField(max_length=71, blank=True, default="")
    manifest = models.JSONField()
    createdAt = models.DateTimeField(auto_now_add=True)

    class Meta:
        constraints = [
            models.UniqueConstraint(
                fields=[
                    "ownerKind",
                    "ownerId",
                    "ownerContentGeneration",
                    "ownerSha256",
                    "processingSpecification",
                ],
                name="unique_representation_input_spec",
            )
        ]
        indexes = [
            models.Index(
                fields=["ownerKind", "ownerId", "ownerContentGeneration"],
                name="representation_owner_identity",
            )
        ]


class KnowledgeSegment(models.Model):
    segmentId = models.CharField(primary_key=True, max_length=96)
    representation = models.ForeignKey(
        DerivedRepresentation, on_delete=models.CASCADE, related_name="segments"
    )
    ordinal = models.PositiveIntegerField()
    boundedText = models.TextField()
    textSha256 = models.CharField(max_length=71)
    locator = models.JSONField()
    createdAt = models.DateTimeField(auto_now_add=True)

    class Meta:
        constraints = [
            models.UniqueConstraint(
                fields=["representation", "ordinal"],
                name="unique_knowledge_segment_ordinal",
            )
        ]


class SessionAssetLink(models.Model):
    id = models.CharField(
        primary_key=True, max_length=64, default=new_session_asset_link_id
    )
    workspace = models.ForeignKey(Workspace, on_delete=models.PROTECT)
    session = models.ForeignKey(
        Session, on_delete=models.PROTECT, related_name="assetLinks"
    )
    sourceObject = models.ForeignKey(
        SourceObject, on_delete=models.PROTECT, null=True, blank=True
    )
    userLibraryObject = models.ForeignKey(
        UserLibraryObject, on_delete=models.PROTECT, null=True, blank=True
    )
    artifact = models.ForeignKey(
        Artifact, on_delete=models.PROTECT, null=True, blank=True
    )
    attachedBy = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT)
    capturedDisplayName = models.CharField(max_length=255)
    capturedContentType = models.CharField(max_length=160)
    capturedOwnerKind = models.CharField(max_length=32)
    capturedOwnerId = models.CharField(max_length=64)
    capturedContentGeneration = models.PositiveBigIntegerField()
    capturedSizeBytes = models.BigIntegerField()
    capturedSha256 = models.CharField(max_length=71)
    createdAt = models.DateTimeField(auto_now_add=True)

    class Meta:
        constraints = [
            models.CheckConstraint(
                check=(
                    models.Q(
                        sourceObject__isnull=False,
                        userLibraryObject__isnull=True,
                        artifact__isnull=True,
                    )
                    | models.Q(
                        sourceObject__isnull=True,
                        userLibraryObject__isnull=False,
                        artifact__isnull=True,
                    )
                    | models.Q(
                        sourceObject__isnull=True,
                        userLibraryObject__isnull=True,
                        artifact__isnull=False,
                    )
                ),
                name="session_asset_link_exactly_one_asset",
            )
        ]

    def save(self, *args, **kwargs):
        if self.workspace_id != self.session.workspace_id:
            raise ValueError("SessionAssetLink workspace/session binding mismatch")
        if self.attachedBy_id != self.session.owner_id:
            raise ValueError("SessionAssetLink attachedBy/session owner mismatch")
        assets = [self.sourceObject, self.userLibraryObject, self.artifact]
        if sum(asset is not None for asset in assets) != 1:
            raise ValueError("SessionAssetLink requires exactly one asset")
        if not self.capturedDisplayName.strip() or not self.capturedContentType.strip():
            raise ValueError("SessionAssetLink requires captured file metadata")
        require_enum(
            "SessionAssetLink.capturedOwnerKind",
            self.capturedOwnerKind,
            {"sourceObject", "userLibraryObject", "artifact"},
        )
        require_sha256("SessionAssetLink.capturedSha256", self.capturedSha256)
        if self.capturedContentGeneration <= 0 or self.capturedSizeBytes < 0:
            raise ValueError("SessionAssetLink requires a positive generation and size")
        if self.sourceObject and self.sourceObject.workspace_id != self.workspace_id:
            raise ValueError("SessionAssetLink source workspace mismatch")
        if (
            self.userLibraryObject
            and self.userLibraryObject.owner_id != self.attachedBy_id
        ):
            raise ValueError("SessionAssetLink library owner mismatch")
        if self.artifact and (
            self.artifact.workspace_id != self.workspace_id
            or self.artifact.session_id != self.session_id
        ):
            raise ValueError("SessionAssetLink artifact scope mismatch")
        owner_kind, owner = next(
            (
                (kind, owner)
                for kind, owner in [
                    ("sourceObject", self.sourceObject),
                    ("userLibraryObject", self.userLibraryObject),
                    ("artifact", self.artifact),
                ]
                if owner is not None
            )
        )
        if self._state.adding and (
            self.capturedOwnerKind != owner_kind
            or self.capturedOwnerId != owner.id
            or self.capturedContentGeneration != owner.contentGeneration
            or self.capturedSizeBytes != owner.sizeBytes
            or self.capturedSha256 != owner.sha256
        ):
            raise ValueError("SessionAssetLink input identity mismatch")
        if not self._state.adding:
            raise ValueError("SessionAssetLink is immutable")
        return super().save(*args, **kwargs)


class MaterialProcessingTask(models.Model):
    """Durable dispatch intent for one shared processed representation."""
    representationId = models.CharField(primary_key=True, max_length=96)
    processingSpecification = models.JSONField()
    payload = models.JSONField()
    status = models.CharField(max_length=16, default="pending", db_index=True)
    errorCode = models.CharField(max_length=96, blank=True, default="")
    executionBackend = models.CharField(max_length=16, default="platform", db_index=True)
    leaseOwner = models.CharField(max_length=128, blank=True, default="")
    leaseExpiresAt = models.DateTimeField(null=True, blank=True)
    leaseEpoch = models.PositiveBigIntegerField(default=0)
    attemptCount = models.PositiveIntegerField(default=0)
    createdAt = models.DateTimeField(auto_now_add=True)
    updatedAt = models.DateTimeField(auto_now=True)


class MaterialProcessor(models.Model):
    name = models.CharField(primary_key=True, max_length=64)
    specification = models.ForeignKey("ProcessingSpecification", on_delete=models.PROTECT)


class MaterialStagedObject(models.Model):
    storageKey = models.CharField(primary_key=True, max_length=1000)
    task = models.ForeignKey(MaterialProcessingTask, on_delete=models.PROTECT)
    sizeBytes = models.PositiveBigIntegerField()
    sha256 = models.CharField(max_length=71)
    createdAt = models.DateTimeField(auto_now_add=True)


class MaterialOperation(models.Model):
    id = models.CharField(primary_key=True, max_length=96)
    agent_run = models.ForeignKey(AgentRun, on_delete=models.CASCADE)
    task = models.ForeignKey(MaterialProcessingTask, on_delete=models.PROTECT, related_name="operations")
    inputRef = models.CharField(max_length=1024)
    cancelledAt = models.DateTimeField(null=True, blank=True)
    createdAt = models.DateTimeField(auto_now_add=True)

    class Meta:
        constraints = [models.UniqueConstraint(fields=["agent_run", "task"], name="unique_run_material_operation")]


class MaterialResultSnapshot(models.Model):
    """Immutable full tool result, retained with the source call and session."""
    id = models.CharField(primary_key=True, max_length=96)
    call = models.OneToOneField(SessionEvent, on_delete=models.CASCADE, related_name="materialResult")
    payload = models.JSONField()
    sha256 = models.CharField(max_length=71)
    createdAt = models.DateTimeField(auto_now_add=True)

    def save(self, *args, **kwargs):
        if not self._state.adding:
            raise ValueError("MaterialResultSnapshot is immutable")
        return super().save(*args, **kwargs)


class MaterialEvidenceReceipt(models.Model):
    id = models.CharField(primary_key=True, max_length=96)
    call = models.OneToOneField(SessionEvent, on_delete=models.CASCADE, related_name="materialReceipt")
    authorizationDigest = models.CharField(max_length=71)
    responseText = models.TextField()
    modelProjection = models.JSONField()
    evidence = models.JSONField()
    createdAt = models.DateTimeField(auto_now_add=True)

    def save(self, *args, **kwargs):
        if not self._state.adding:
            raise ValueError("MaterialEvidenceReceipt is immutable")
        return super().save(*args, **kwargs)


class ModelRunLog(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_model_run_id)
    agentRunId = models.CharField(max_length=64)
    modelConfig = models.ForeignKey(ModelConfig, on_delete=models.PROTECT)
    status = models.CharField(max_length=32)
    promptTokens = models.IntegerField(null=True, blank=True)
    completionTokens = models.IntegerField(null=True, blank=True)
    totalTokens = models.IntegerField(null=True, blank=True)
    promptCacheHitTokens = models.IntegerField(null=True, blank=True)
    promptCacheMissTokens = models.IntegerField(null=True, blank=True)
    error = models.TextField(blank=True, default="")
    createdAt = models.DateTimeField(auto_now_add=True)


class BrowserLoginSession(AbstractBaseSession):
    """Django login credentials; a null deadline persists until revocation."""
    expire_date = models.DateTimeField("expire date", null=True, blank=True, db_index=True)

    class Meta:
        verbose_name = "browser login"
        verbose_name_plural = "browser logins"

    @classmethod
    def get_session_store_class(cls):
        from .persistent_sessions import SessionStore
        return SessionStore


class StorageCleanupRetry(models.Model):
    """Retry ownership for exact storage keys without a DerivedResource owner."""

    keyDigest = models.CharField(primary_key=True, max_length=64)
    storageKey = models.CharField(max_length=1000)
    collector = models.CharField(max_length=32)
    state = models.CharField(max_length=32, default="pending")
    cleanupAttempts = models.PositiveIntegerField(default=0)
    nextCleanupAt = models.DateTimeField(null=True, blank=True)
    quarantinedAt = models.DateTimeField(null=True, blank=True)
    leaseOwner = models.CharField(max_length=96, blank=True, default="")
    leaseExpiresAt = models.DateTimeField(null=True, blank=True)
    lastFailure = models.TextField(blank=True, default="")
    cleanedAt = models.DateTimeField(null=True, blank=True)
    createdAt = models.DateTimeField(auto_now_add=True)
    updatedAt = models.DateTimeField(auto_now=True)

    def save(self, *args, **kwargs):
        require_enum("StorageCleanupRetry.collector", self.collector, {"workspaceSnapshot", "orphanedLibrary"})
        require_enum("StorageCleanupRetry.state", self.state, {"pending", "cleaning", "failed", "cleaned", "quarantined"})
        return super().save(*args, **kwargs)
