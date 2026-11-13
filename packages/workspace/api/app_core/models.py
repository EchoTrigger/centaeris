import secrets
import unicodedata
import uuid
from urllib.parse import urlsplit

from django.conf import settings
from django.contrib.sessions.base_session import AbstractBaseSession
from django.contrib.postgres.indexes import GinIndex, OpClass
from django.core.exceptions import ValidationError
from django.core.validators import validate_email
from django.db import IntegrityError, models, router, transaction
from django.db.models.functions import Lower
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


def new_external_identity_id() -> str:
    return new_id("external_identity")


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


def new_agent_run_api_credential_id() -> str:
    return new_id("api_credential")


def new_agent_run_api_conversation_id() -> str:
    return new_id("conversation")


# Referenced by migration 0044.
new_one_shot_credential_id = new_agent_run_api_credential_id


def new_credential_audit_id() -> str:
    return new_id("credential_audit")


def new_mcp_bearer_credential_id() -> str:
    return new_id("mcp_credential")


def new_mcp_credential_audit_id() -> str:
    return new_id("mcp_credential_audit")


def new_session_id() -> str:
    return f"session_{secrets.token_urlsafe(12)}"


def new_session_environment_id() -> str:
    return new_id("environment")


def new_environment_release_id() -> str:
    return new_id("environment_release")


def new_environment_maintenance_grant_id() -> str:
    return new_id("maintenance_grant")


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
    avatar_image = models.BinaryField(null=True, blank=True)
    avatar_digest = models.CharField(max_length=64, blank=True, default="")
    description = models.TextField(blank=True, default="")
    execution_network = models.CharField(max_length=14, default="disabled",
        choices=[("disabled", "Disabled"), ("publicInternet", "Internet")])
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

    class Meta:
        constraints = [models.CheckConstraint(condition=models.Q(execution_network__in=["disabled", "publicInternet"]),
            name="workspace_execution_network_valid"),
            models.CheckConstraint(
                condition=(models.Q(avatar_image__isnull=True, avatar_digest="")
                    | models.Q(avatar_image__isnull=False, avatar_digest__regex=r"^[0-9a-f]{64}$")),
                name="workspace_avatar_image_consistent",
            )]

    def save(self, *args, **kwargs):
        require_enum("Workspace.status", self.status, {"active", "archived"})
        require_enum("Workspace.execution_network", self.execution_network, {"disabled", "publicInternet"})
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


class UserWorkspaceGuideState(models.Model):
    user = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.CASCADE)
    workspace = models.ForeignKey(Workspace, on_delete=models.CASCADE)
    dismissedAt = models.DateTimeField(null=True, blank=True)

    class Meta:
        constraints = [models.UniqueConstraint(
            fields=["user", "workspace"], name="user_workspace_guide_unique",
        )]


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


class WorkspacePluginInstallation(models.Model):
    """A workspace pins immutable package bytes; it never replaces the platform catalog."""
    workspace = models.ForeignKey(Workspace, on_delete=models.PROTECT, related_name="plugin_installations")
    plugin_name = models.CharField(max_length=64)
    package = models.JSONField()
    updated_by = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT)
    updated_at = models.DateTimeField(auto_now=True)

    class Meta:
        constraints = [models.UniqueConstraint(fields=["workspace", "plugin_name"], name="workspace_plugin_installation_unique")]


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
    # NULL is the explicit platform/shared scope preserved for released data.
    workspace = models.ForeignKey(Workspace, null=True, blank=True, on_delete=models.PROTECT, related_name="model_providers")
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
                condition=models.Q(template_id__isnull=False, archivedAt__isnull=True, workspace__isnull=True),
                name="model_provider_active_template_unique",
            ),
            models.UniqueConstraint(fields=["workspace", "template_id"], condition=models.Q(template_id__isnull=False, archivedAt__isnull=True, workspace__isnull=False), name="workspace_provider_template_unique"),
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


class ProviderCredential(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_credential_id)
    provider = models.OneToOneField(
        ModelProvider,
        on_delete=models.PROTECT,
        related_name="credential",
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
            models.CheckConstraint(condition=models.Q(credential_version__gt=0), name="app_delegation_version_positive"),
        ]

    def save(self, *args, **kwargs):
        raise ValueError("Agent-target application grants are retired")


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


class McpBearerCredential(models.Model):
    workspace = models.ForeignKey(Workspace, null=True, blank=True, on_delete=models.PROTECT, related_name="mcp_credentials")
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
                condition=models.Q(workspace__isnull=True),
                name="mcp_bearer_credential_plugin_ref_unique",
            ),
            models.UniqueConstraint(fields=["workspace", "plugin_name", "credential_ref"], condition=models.Q(workspace__isnull=False), name="workspace_mcp_credential_unique")
        ]

    def save(self, *args, **kwargs):
        validate_lower_kebab("MCP plugin name", self.plugin_name)
        validate_lower_kebab("MCP bearer credential ref", self.credential_ref)
        validate_display_name(self.display_name)
        if not self.encrypted_secret or self.version <= 0:
            raise ValueError("MCP bearer credential secret and version are required")
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
    name = models.CharField(max_length=100)
    created_at = models.DateTimeField(auto_now_add=True)


class Session(models.Model):
    objects = ResourceQuerySet.as_manager()
    id = models.CharField(primary_key=True, max_length=64, default=new_session_id)
    purpose = models.CharField(max_length=16, default="interactive")
    workspace = models.ForeignKey(
        Workspace, on_delete=models.PROTECT, related_name="sessions"
    )
    owner = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT)
    project = models.ForeignKey(
        SessionProject,
        db_column="project_id",
        on_delete=models.PROTECT,
        related_name="sessions",
        null=True,
        blank=True,
    )
    # Nullable only while this Session and its initial Environment are inserted
    # in one transaction. PostgreSQL checks the completed binding at commit.
    current_environment = models.ForeignKey(
        "SessionEnvironment", on_delete=models.PROTECT, related_name="+",
        null=True, blank=True, editable=False,
    )
    title = models.CharField(max_length=200, default="New chat")
    origin = models.CharField(max_length=32, default="user")
    status = models.CharField(max_length=32, default="active")
    archivedAt = models.DateTimeField(null=True, blank=True)
    deletedAt = models.DateTimeField(null=True, blank=True)
    deletedBy = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.PROTECT,
        related_name="+",
        null=True,
        blank=True,
    )
    purgedAt = models.DateTimeField(null=True, blank=True)
    purgeRequestedAt = models.DateTimeField(null=True, blank=True, editable=False)
    isPinned = models.BooleanField(default=False)
    # Explicit owner action only; automatic notification state is derived from watermarks.
    isManuallyUnread = models.BooleanField(default=False)
    attentionRevision = models.PositiveBigIntegerField(default=0, db_default=0)
    latestAttentionSequence = models.PositiveBigIntegerField(default=0, db_default=0)
    readThroughSequence = models.PositiveBigIntegerField(default=0, db_default=0)
    workspaceGeneration = models.PositiveBigIntegerField(default=0)
    workspaceBaselineKnown = models.BooleanField(default=True, db_default=True, editable=False)
    workspaceStorageKey = models.CharField(max_length=1000, blank=True, default="")
    workspaceSnapshotSha256 = models.CharField(max_length=71, blank=True, default="")
    workspaceSnapshotEncoding = models.CharField(max_length=16, default="frameV1")
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
        indexes = [GinIndex(OpClass(Lower("title"), name="gin_bigm_ops"), name="session_title_bigm")]
        constraints = [
            models.CheckConstraint(
                condition=models.Q(purpose__in=("interactive", "agentRunApi")),
                name="session_purpose_valid",
            ),
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
                condition=(models.Q(workspaceBaselineKnown=True) | models.Q(workspaceGeneration=0)),
                name="session_workspace_authority_consistent",
            ),
            models.CheckConstraint(
                condition=(
                    models.Q(workspaceSnapshotEncoding__in=["frameV1", "tarZstdV1", "fileManifestV1"])
                    & (models.Q(workspaceSnapshotEncoding="frameV1")
                       | models.Q(workspaceSnapshotSizeBytes__gt=0))
                ),
                name="session_workspace_encoding_valid",
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
        if not self._state.adding:
            stored_scope = type(self).objects.values_list(
                "workspace_id", "owner_id", "current_environment_id", "current_environment__session_id",
            ).get(pk=self.pk)
            if stored_scope[:2] != (self.workspace_id, self.owner_id):
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
        using = kwargs.get("using") or router.db_for_write(type(self), instance=self)
        if self._state.adding:
            if self.current_environment_id is not None:
                raise ValueError("new_session_requires_new_environment")
            previous_database = self._state.db
            try:
                with transaction.atomic(using=using):
                    kwargs["force_insert"] = True
                    result = super().save(*args, **kwargs)
                    environment = SessionEnvironment.objects.using(using).create(session=self)
                    type(self).objects.using(using).filter(pk=self.pk).update(current_environment=environment)
                    self.current_environment = environment
                return result
            except BaseException:
                # save() mutates the instance before the transaction commits.
                # Retain its identity so an uncommitted creation can be retried.
                # An uncertain commit still hits the primary-key constraint.
                self._state.adding, self._state.db = True, previous_database
                self.current_environment = None
                raise
        if not self.current_environment_id:
            raise ValueError("session_environment_binding_invalid")
        if self.current_environment_id == stored_scope[2]:
            if stored_scope[3] != self.pk:
                raise ValueError("session_environment_binding_invalid")
        elif not SessionEnvironment.objects.using(using).filter(pk=self.current_environment_id, session_id=self.pk).exists():
            raise ValueError("session_environment_binding_invalid")
        return super().save(*args, **kwargs)


class SessionEnvironment(models.Model):
    """A Session's business identity and monotonically fenced controller authority."""

    id = models.CharField(primary_key=True, max_length=64, default=new_session_environment_id)
    session = models.ForeignKey(Session, on_delete=models.PROTECT, related_name="environments")
    created_at = models.DateTimeField(auto_now_add=True)
    network = models.CharField(max_length=14, default="disabled", editable=False,
        choices=[("disabled", "Disabled"), ("publicInternet", "Internet")])
    environment_profile = models.JSONField(null=True, default=None, blank=True, editable=False)
    environment_profile_digest = models.CharField(max_length=71, default="", blank=True, editable=False)
    environment_profile_operation_id = models.CharField(max_length=128, default="", blank=True, editable=False)
    environment_profile_bound_at = models.DateTimeField(null=True, default=None, editable=False)
    historical_profile = models.JSONField(null=True, default=None, blank=True, editable=False)
    benchmark_setup_operation_id = models.CharField(max_length=128, default="", blank=True, editable=False)
    benchmark_setup_run = models.ForeignKey("AgentRun", on_delete=models.PROTECT,
        related_name="+", null=True, default=None, editable=False)
    benchmark_setup_instance_id = models.CharField(max_length=73, default="", blank=True, editable=False)
    benchmark_setup_container_id = models.CharField(max_length=64, default="", blank=True, editable=False)
    benchmark_setup_completed_at = models.DateTimeField(null=True, default=None, editable=False)
    controller_epoch = models.PositiveBigIntegerField(default=0, editable=False)
    controller_run = models.ForeignKey("AgentRun", on_delete=models.PROTECT,
        related_name="+", null=True, blank=True, editable=False)
    controller_maintenance_operation = models.ForeignKey("EnvironmentMaintenanceOperation",
        on_delete=models.PROTECT, related_name="+", null=True, blank=True, editable=False)
    controller_lease_owner = models.CharField(max_length=160, blank=True, default="", editable=False)
    controller_released = models.BooleanField(default=True, editable=False)
    controller_purpose = models.CharField(max_length=7, default="", editable=False,
        choices=[("execute", "Execute"), ("cleanup", "Cleanup")])

    class Meta:
        constraints = [models.CheckConstraint(condition=models.Q(network__in=["disabled", "publicInternet"]),
            name="environment_network_valid"), models.CheckConstraint(
            condition=(models.Q(controller_run__isnull=True, controller_maintenance_operation__isnull=True,
                               controller_lease_owner="", controller_released=True, controller_purpose="")
                | (((models.Q(controller_run__isnull=False, controller_maintenance_operation__isnull=True)
                     | models.Q(controller_run__isnull=True, controller_maintenance_operation__isnull=False))
                    & models.Q(controller_epoch__gt=0, controller_purpose__in=["execute", "cleanup"]))
                   & ~models.Q(controller_lease_owner=""))),
            name="environment_controller_complete"), models.CheckConstraint(
            condition=(models.Q(environment_profile__isnull=True, environment_profile_digest="",
                environment_profile_operation_id="", environment_profile_bound_at__isnull=True)
                | (models.Q(environment_profile__isnull=False, environment_profile_bound_at__isnull=False)
                   & ~models.Q(environment_profile_digest="") & ~models.Q(environment_profile_operation_id=""))),
            name="environment_profile_complete"), models.CheckConstraint(
            condition=(models.Q(benchmark_setup_operation_id="", benchmark_setup_run__isnull=True,
                benchmark_setup_instance_id="", benchmark_setup_container_id="", benchmark_setup_completed_at__isnull=True)
                | (models.Q(environment_profile__isnull=False, benchmark_setup_run__isnull=False,
                            benchmark_setup_completed_at__isnull=False)
                   & ~models.Q(benchmark_setup_operation_id="") & ~models.Q(benchmark_setup_instance_id="")
                   & ~models.Q(benchmark_setup_container_id=""))),
            name="environment_benchmark_setup_complete")]

    def save(self, *args, **kwargs):
        if self._state.adding and self.environment_profile is not None:
            raise ValueError("environment_profile_requires_binding")
        using = kwargs.get("using") or router.db_for_write(type(self), instance=self)
        if self._state.adding:
            self.network = Workspace.objects.using(using).get(sessions__pk=self.session_id).execution_network
        require_enum("SessionEnvironment.network", self.network, {"disabled", "publicInternet"})
        if not self._state.adding:
            using = kwargs.get("using") or router.db_for_write(type(self), instance=self)
            stored = type(self).objects.using(using).values("session_id", "network", "environment_profile",
                "environment_profile_digest", "environment_profile_operation_id", "environment_profile_bound_at", "historical_profile",
                "benchmark_setup_operation_id", "benchmark_setup_run_id", "benchmark_setup_instance_id",
                "benchmark_setup_container_id", "benchmark_setup_completed_at").get(pk=self.pk)
            if stored["session_id"] != self.session_id:
                raise ValueError("session_environment_identity_immutable")
            if stored["historical_profile"] != self.historical_profile:
                raise ValueError("environment_profile_history_immutable")
            binding_profile = stored["environment_profile"] is None and self.environment_profile is not None
            if stored["network"] != self.network and not binding_profile:
                raise ValueError("environment_network_immutable")
            fields = ("environment_profile", "environment_profile_digest", "environment_profile_operation_id", "environment_profile_bound_at")
            if stored["environment_profile"] is not None and any(stored[name] != getattr(self, name) for name in fields):
                raise ValueError("environment_profile_binding_immutable")
            setup_fields = ("benchmark_setup_operation_id", "benchmark_setup_run_id", "benchmark_setup_instance_id",
                            "benchmark_setup_container_id", "benchmark_setup_completed_at")
            if stored["benchmark_setup_operation_id"] and any(stored[name] != getattr(self, name) for name in setup_fields):
                raise ValueError("benchmark_setup_binding_immutable")
            if (stored["environment_profile"] is None and self.environment_profile is not None
                    and AgentRunAuthorization.objects.using(using).filter(agent_run__environment_id=self.pk).exists()):
                raise ValueError("environment_profile_requires_unused_session")
        if self.environment_profile is not None:
            from .environment_profile import environment_profile_digest
            if environment_profile_digest(self.environment_profile) != self.environment_profile_digest:
                raise ValueError("environment_profile_digest_invalid")
            if self.network != self.environment_profile["network"]:
                raise ValueError("environment_network_profile_mismatch")
        created = self._state.adding
        result = super().save(*args, **kwargs)
        if created:
            # PostgreSQL snapshots the authoritative default at INSERT time.
            # A concurrent settings update may have superseded our earlier read.
            self.refresh_from_db(using=using, fields=["network"])
        return result


class EnvironmentMaintenanceOperation(models.Model):
    """One immutable request and physical target, independently owned from a Run."""
    operation_id = models.CharField(primary_key=True, max_length=160)
    operation_kind = models.CharField(max_length=9, default='task', choices=[('task', 'Task'), ('provision', 'Provision')])
    workspace = models.ForeignKey(Workspace, on_delete=models.PROTECT, related_name="+")
    session = models.ForeignKey(Session, on_delete=models.PROTECT, related_name="+")
    environment = models.ForeignKey(SessionEnvironment, on_delete=models.PROTECT, related_name="maintenance_operations")
    instance_id = models.CharField(max_length=73, null=True)
    container_id = models.CharField(max_length=64, null=True)
    host_id = models.CharField(max_length=160, null=True)
    allocation_execution_id = models.TextField(null=True)
    profile_digest = models.CharField(max_length=71)
    request = models.JSONField()
    request_digest = models.CharField(max_length=71)
    current_grant = models.ForeignKey("EnvironmentMaintenanceGrant", on_delete=models.PROTECT,
        related_name="+", null=True, editable=False)
    state = models.CharField(max_length=8, default="active", choices=[("active", "Active"), ("released", "Released")])
    released_at = models.DateTimeField(null=True, editable=False)

    class Meta:
        constraints = [models.CheckConstraint(
            condition=models.Q(state="active", released_at__isnull=True)
                | models.Q(state="released", released_at__isnull=False), name="maintenance_operation_state_valid"),
            models.CheckConstraint(condition=(models.Q(operation_kind='provision', instance_id__isnull=True,
                container_id__isnull=True, host_id__isnull=True, allocation_execution_id__isnull=True)
                | (models.Q(operation_kind='task', instance_id__isnull=False, container_id__isnull=False,
                    host_id__isnull=False, allocation_execution_id__isnull=False)
                    & ~models.Q(instance_id='') & ~models.Q(container_id='') & ~models.Q(host_id='')
                    & ~models.Q(allocation_execution_id=''))), name='maintenance_operation_target_valid')]

    def save(self, *args, **kwargs):
        if self._state.adding:
            kwargs['force_insert'] = True
        else:
            fields = ("operation_kind", "workspace_id", "session_id", "environment_id", "instance_id", "container_id", "host_id",
                      "allocation_execution_id", "profile_digest", "request", "request_digest")
            stored = type(self).objects.values(*fields).get(pk=self.pk)
            if any(stored[name] != getattr(self, name) for name in fields):
                raise ValueError("maintenance_operation_immutable")
        return super().save(*args, **kwargs)


class EnvironmentMaintenanceGrant(models.Model):
    """Append-only signatures; cleanup renewal never rewrites execution authority."""
    id = models.CharField(primary_key=True, max_length=64, default=new_environment_maintenance_grant_id)
    operation = models.ForeignKey(EnvironmentMaintenanceOperation, on_delete=models.PROTECT, related_name="grants")
    previous_controller_epoch = models.PositiveBigIntegerField()
    controller_epoch = models.PositiveBigIntegerField()
    controller_lease_digest = models.CharField(max_length=71)
    controller_purpose = models.CharField(max_length=7, choices=[("execute", "Execute"), ("cleanup", "Cleanup")])
    expires_at_ms = models.PositiveBigIntegerField()
    payload = models.JSONField()
    digest = models.CharField(max_length=71, default='', editable=False)
    signature = models.CharField(max_length=76)

    class Meta:
        constraints = [models.UniqueConstraint(fields=("operation", "controller_epoch"), name="maintenance_grant_epoch_unique"),
            models.UniqueConstraint(fields=("operation", "previous_controller_epoch", "controller_purpose"),
                                    name="maintenance_grant_request_unique"),
            models.CheckConstraint(condition=models.Q(controller_epoch__gt=0, expires_at_ms__gt=0,
                controller_purpose__in=["execute", "cleanup"]), name="maintenance_grant_valid"),
            models.CheckConstraint(condition=models.Q(digest__regex=r'^sha256:[0-9a-f]{64}$'),
                name='maintenance_grant_digest_valid')]

    def save(self, *args, **kwargs):
        if not self._state.adding:
            raise ValueError("maintenance_grant_immutable")
        from .environment_maintenance import _digest
        digest = _digest(self.payload)
        if self.digest not in ('', digest):
            raise ValueError('maintenance_grant_digest_invalid')
        self.digest = digest
        kwargs['force_insert'] = True
        return super().save(*args, **kwargs)


class AgentRunEnvironmentRelease(models.Model):
    """Immutable business release acknowledgement, not physical cleanup evidence."""
    id = models.CharField(primary_key=True, max_length=64, default=new_environment_release_id)
    agent_run = models.OneToOneField("AgentRun", on_delete=models.PROTECT, related_name="environment_release")
    environment = models.ForeignKey(SessionEnvironment, on_delete=models.PROTECT, related_name="releases")
    authorization_digest = models.CharField(max_length=71)
    controller_epoch = models.PositiveBigIntegerField()
    controller_lease_digest = models.CharField(max_length=71)
    recorded_at = models.DateTimeField(auto_now_add=True)
    reconstructed = models.BooleanField(default=False)

    class Meta:
        constraints = [models.CheckConstraint(condition=models.Q(controller_epoch__gt=0),
            name="environment_release_epoch_positive")]

    def save(self, *args, **kwargs):
        if not self._state.adding:
            raise ValueError("environment_release_immutable")
        return super().save(*args, **kwargs)


class AgentRun(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_agent_run_id)
    turn_id = models.CharField(max_length=64, default=new_turn_id, unique=True)
    membership_ref = models.CharField(max_length=64, editable=False)
    workspace = models.ForeignKey(Workspace, on_delete=models.PROTECT)
    session = models.ForeignKey(
        Session, on_delete=models.PROTECT, related_name="agent_runs"
    )
    environment = models.ForeignKey(
        SessionEnvironment, on_delete=models.PROTECT, related_name="agent_runs", editable=False,
    )
    user = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT)
    acting_app = models.ForeignKey(BusinessApplication, on_delete=models.PROTECT, related_name="agent_runs", null=True, blank=True)
    app_delegation = models.ForeignKey(UserAppDelegation, on_delete=models.PROTECT, related_name="agent_runs", null=True, blank=True)
    modelConfig = models.ForeignKey(ModelConfig, on_delete=models.PROTECT)
    thinkingMode = models.CharField(max_length=64, blank=True, default="")
    prompt = models.TextField()
    agent_instructions = models.TextField(blank=True, default="")
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
            models.Index(fields=["workspace", "id"],
                         condition=models.Q(status__in=["queued", "running"]),
                         name="agent_run_active_admission"),
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
        if self.modelConfig_id and self.modelConfig.provider_id and self.modelConfig.provider.workspace_id not in (None,self.workspace_id):
            raise ValueError("AgentRun model workspace mismatch")
        if (self.acting_app_id is None) != (self.app_delegation_id is None):
            raise ValueError("AgentRun application origin is incomplete")
        if self._state.adding and self.app_delegation_id:
            raise ValueError("Agent-target application grants are retired")
        if not self._state.adding:
            stored_snapshot = type(self).objects.values_list(
                "acting_app_id", "app_delegation_id", "agent_instructions", "session_id", "environment_id",
                "modelConfig_id", "thinkingMode",
            ).get(pk=self.pk)
            if stored_snapshot[:3] != (self.acting_app_id, self.app_delegation_id, self.agent_instructions):
                raise ValueError("AgentRun configuration snapshot is immutable")
            if stored_snapshot[5:] != (self.modelConfig_id, self.thinkingMode):
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
        using = kwargs.get("using") or router.db_for_write(type(self), instance=self)
        if self._state.adding:
            with transaction.atomic(using=using):
                session = Session.objects.using(using).select_for_update(of=("self",)).select_related(
                    "current_environment",
                ).get(pk=self.session_id)
                if not session.current_environment_id:
                    raise ValueError("session_environment_binding_missing")
                if self.environment_id is not None and self.environment_id != session.current_environment_id:
                    raise ValueError("agent_run_environment_binding_invalid")
                self.environment = session.current_environment
                return super().save(*args, **kwargs)
        if stored_snapshot[3:5] != (self.session_id, self.environment_id):
            raise ValueError("agent_run_environment_binding_immutable")
        return super().save(*args, **kwargs)


class ConversationResetPolicy(models.TextChoices):
    NEVER = "never"
    ON_REOPEN = "onReopen"
    AFTER_10_MINUTES = "after10Minutes"
    AFTER_15_MINUTES = "after15Minutes"
    AFTER_30_MINUTES = "after30Minutes"
    AFTER_60_MINUTES = "after60Minutes"


class AgentRunApiCredential(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_agent_run_api_credential_id)
    workspace = models.ForeignKey(Workspace, on_delete=models.PROTECT)
    user = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT)
    membership_ref = models.CharField(max_length=64)
    name = models.CharField(max_length=120)
    modelConfig = models.ForeignKey(ModelConfig, on_delete=models.PROTECT)
    pluginNames = models.JSONField(default=list)
    sourceObjectRefs = models.JSONField(default=list)
    secretDigest = models.CharField(max_length=64, unique=True)
    encryptedSecret = models.TextField(blank=True, default="")
    resetPolicy = models.CharField(max_length=16, choices=ConversationResetPolicy, default=ConversationResetPolicy.NEVER)
    createdAt = models.DateTimeField(auto_now_add=True)


class AgentRunApiConversation(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_agent_run_api_conversation_id)
    credential = models.ForeignKey(AgentRunApiCredential, on_delete=models.SET_NULL, null=True)
    sessionId = models.CharField(max_length=64, unique=True)
    closedAt = models.DateTimeField(null=True, blank=True)
    supersededById = models.CharField(max_length=64, blank=True, default="")
    purgedAt = models.DateTimeField(null=True, blank=True)


class AgentRunApiOperation(models.Model):
    credential = models.ForeignKey(AgentRunApiCredential, on_delete=models.SET_NULL, null=True)
    operationId = models.CharField(max_length=128)
    requestDigest = models.CharField(max_length=64)
    # Keep acceptance independent of temporary execution records and their erasure.
    agentRunId = models.CharField(max_length=64, unique=True)
    sessionId = models.CharField(max_length=64, db_index=True)
    conversationId = models.CharField(max_length=64, blank=True, default="")
    acceptedAt = models.DateTimeField(auto_now_add=True)
    deadlineAt = models.DateTimeField()
    expiresAt = models.DateTimeField()
    runStatus = models.CharField(max_length=16, default="queued")
    cleanupStatus = models.CharField(max_length=16, default="pending")
    reasonCode = models.CharField(max_length=160, blank=True, default="")
    deliveryLeaseUntil = models.DateTimeField(null=True, blank=True)
    deliveryClosedAt = models.DateTimeField(null=True, blank=True)

    class Meta:
        constraints = [
            models.UniqueConstraint(fields=["credential", "operationId"], name="agent_run_api_operation_unique"),
            models.CheckConstraint(condition=models.Q(runStatus__in=("queued", "running", "completed", "failed", "cancelled")),
                                   name="agent_run_api_run_status_valid"),
            models.CheckConstraint(condition=models.Q(cleanupStatus__in=("pending", "cleaning", "completed", "failed", "retained")),
                                   name="agent_run_api_cleanup_status_valid"),
        ]
        indexes = [models.Index(fields=["cleanupStatus", "acceptedAt"], name="agent_run_api_cleanup_due")]


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
                    | models.Q(command__in=["submitMessage", "consumeWorkReturn", "respondQuestion"], agentRunId__isnull=False, turnId__isnull=False)
                    | models.Q(command="consumeWorkReturn", agentRunId__isnull=True, turnId__isnull=True)
                ),
                name="hosted_operation_result_valid",
            ),
        ]


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


class SessionWorkspacePreservation(models.Model):
    """Immutable proof that one exact stopped allocation reached Session storage."""
    id = models.CharField(primary_key=True, max_length=71)
    session = models.ForeignKey(Session, on_delete=models.PROTECT, related_name="workspacePreservations")
    agent_run = models.ForeignKey(AgentRun, on_delete=models.PROTECT, related_name="workspacePreservations")
    origin_agent_run = models.ForeignKey(AgentRun, on_delete=models.PROTECT, related_name="originWorkspacePreservations")
    environment = models.ForeignKey(SessionEnvironment, on_delete=models.PROTECT, related_name="workspacePreservations")
    authorization_digest = models.CharField(max_length=71)
    origin_authorization_digest = models.CharField(max_length=71)
    allocation_execution_id = models.CharField(max_length=160)
    instance_id = models.CharField(max_length=80)
    container_id = models.CharField(max_length=64)
    profile_digest = models.CharField(max_length=71)
    baseline = models.JSONField()
    workspace = models.JSONField()
    snapshot_encoding = models.CharField(max_length=16, default="frameV1")
    storage_key = models.CharField(max_length=1000, blank=True)
    creator_lease_digest = models.CharField(max_length=71)
    creator_controller_epoch = models.PositiveBigIntegerField()
    created_at = models.DateTimeField(auto_now_add=True)

    def save(self, *args, **kwargs):
        if not self._state.adding:
            raise ValueError("session_workspace_preservation_immutable")
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
        indexes = [
            GinIndex(OpClass(Lower(models.Func("payload", function="workspace_session_search_text",
                output_field=models.TextField())), name="gin_bigm_ops"), name="session_event_body_bigm",
                condition=models.Q(payload__type__in=["user_message", "phase_event", "assistant_message"])),
            models.Index(fields=["insertedAt", "eventId"], name="agent_work_recovery_scan",
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
    noteHead = models.ForeignKey(
        "LibraryNoteRevision", on_delete=models.PROTECT, null=True, blank=True,
        related_name="currentFor",
    )
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


class LibraryNoteRevision(models.Model):
    note = models.ForeignKey(UserLibraryObject, on_delete=models.CASCADE, related_name="noteRevisions")
    generation = models.PositiveBigIntegerField()
    displayName = models.CharField(max_length=255)
    markdown = models.TextField()
    sha256 = models.CharField(max_length=71)
    sizeBytes = models.PositiveBigIntegerField()
    protected = models.BooleanField(default=False)
    createdAt = models.DateTimeField(auto_now_add=True)
    createdBy = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT, related_name="+")

    class Meta:
        constraints = [models.UniqueConstraint(fields=["note", "generation"], name="unique_note_revision_generation")]

    def save(self, *args, **kwargs):
        if not self._state.adding:
            raise ValueError("note_revision_immutable")
        super().save(*args, **kwargs)


class LibraryNoteSaveReceipt(models.Model):
    owner = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT, related_name="+")
    saveId = models.CharField(max_length=64)
    requestDigest = models.CharField(max_length=64)
    noteId = models.CharField(max_length=64)
    appliedGeneration = models.PositiveBigIntegerField()
    resultMetadata = models.JSONField()
    createdAt = models.DateTimeField(auto_now_add=True)

    class Meta:
        constraints = [models.UniqueConstraint(fields=["owner", "saveId"], name="unique_note_save_receipt")]


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


class RunMaterialScope(models.Model):
    agent_run = models.OneToOneField(AgentRun, on_delete=models.CASCADE, primary_key=True)
    personal_library = models.BooleanField(default=False)

    def save(self, *args, **kwargs):
        if not self._state.adding:
            raise ValueError("RunMaterialScope is immutable")
        return super().save(*args, **kwargs)


class SessionMaterialInput(models.Model):
    id = models.CharField(primary_key=True, max_length=96)
    session = models.ForeignKey(Session, on_delete=models.CASCADE, related_name="materialInputs")
    identity_key = models.CharField(max_length=64)
    input_ref = models.CharField(max_length=1024)
    resolved = models.JSONField()
    confirmed = models.BooleanField(default=False)

    class Meta:
        constraints = [
            models.UniqueConstraint(fields=["session", "identity_key"], name="session_material_identity_unique"),
            models.UniqueConstraint(fields=["session", "input_ref"], name="session_material_ref_unique"),
        ]


class RunMaterialAdmission(models.Model):
    agent_run = models.ForeignKey(AgentRun, on_delete=models.CASCADE)
    material_input = models.ForeignKey(SessionMaterialInput, on_delete=models.CASCADE, related_name="admissions")
    origin_call = models.ForeignKey(SessionEvent, on_delete=models.SET_NULL, null=True)
    confirmed = models.BooleanField(default=False)
    settled_absent = models.BooleanField(default=False)

    class Meta:
        constraints = [models.UniqueConstraint(fields=["agent_run", "material_input", "origin_call"], name="run_material_admission_unique")]


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


class ExternalIdentity(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_external_identity_id)
    user = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT, related_name="external_identities")
    provider_id = models.CharField(max_length=64)
    provider_name = models.CharField(max_length=160)
    provider_fingerprint = models.CharField(max_length=64)
    subject = models.CharField(max_length=255)
    display_name = models.CharField(max_length=150, blank=True)
    created_at = models.DateTimeField(auto_now_add=True)
    last_login_at = models.DateTimeField(null=True, blank=True)

    class Meta:
        constraints = [
            models.UniqueConstraint(fields=["provider_id", "subject"], name="external_identity_subject_unique"),
            models.UniqueConstraint(fields=["user", "provider_id"], name="external_identity_user_provider_unique"),
            models.CheckConstraint(condition=~models.Q(subject="") & ~models.Q(provider_id=""), name="external_identity_nonempty"),
        ]


class SsoAuthorizationAttempt(models.Model):
    state_digest = models.CharField(primary_key=True, max_length=64)
    provider_id = models.CharField(max_length=64)
    provider_fingerprint = models.CharField(max_length=64)
    purpose = models.CharField(max_length=5)
    actor = models.ForeignKey(settings.AUTH_USER_MODEL, null=True, on_delete=models.CASCADE)
    actor_auth_hash = models.CharField(max_length=64, blank=True)
    session_digest = models.CharField(max_length=64)
    return_to = models.CharField(max_length=2048)
    expires_at = models.DateTimeField(db_index=True)
    consumed_at = models.DateTimeField(null=True)

    class Meta:
        constraints = [models.CheckConstraint(condition=models.Q(purpose__in=["login", "link"]), name="sso_attempt_purpose_valid")]


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


def new_workspace_publication_token():
    return uuid.uuid4().hex


class WorkspaceSnapshotObject(models.Model):
    """An exact Session-owned object key and its current publication incarnation."""
    keyDigest = models.CharField(primary_key=True, max_length=64)
    storageKey = models.CharField(max_length=1000, unique=True)
    session = models.ForeignKey(Session, on_delete=models.PROTECT, related_name="workspaceSnapshotObjects")
    kind = models.CharField(max_length=16)
    snapshotEncoding = models.CharField(max_length=16, default="frameV1")
    state = models.CharField(max_length=16, default="live")
    publicationToken = models.CharField(max_length=32, default=new_workspace_publication_token)
    registered = models.BooleanField(default=False)
    collectedAt = models.DateTimeField(null=True, blank=True)
    createdAt = models.DateTimeField(auto_now_add=True)
    updatedAt = models.DateTimeField(auto_now=True)

    class Meta:
        constraints = [
            models.CheckConstraint(condition=models.Q(kind__in=["snapshot", "temporary", "file", "filePack"]), name="workspace_object_kind"),
            models.CheckConstraint(condition=(models.Q(state="live", collectedAt__isnull=True)
                | models.Q(state="collected", collectedAt__isnull=False)), name="workspace_object_state"),
        ]


class WorkspaceSnapshotUpload(models.Model):
    """Durable ownership of streaming and staged bytes before a recovery reference."""
    id = models.CharField(primary_key=True, max_length=32)
    finalObject = models.ForeignKey(WorkspaceSnapshotObject, on_delete=models.PROTECT, related_name="finalUploads")
    temporaryObject = models.OneToOneField(WorkspaceSnapshotObject, on_delete=models.PROTECT, related_name="temporaryUpload")
    agent_run = models.ForeignKey(AgentRun, on_delete=models.PROTECT, related_name="workspaceSnapshotUploads")
    jobId = models.CharField(max_length=256)
    leaseOwner = models.CharField(max_length=160)
    authorizationDigest = models.CharField(max_length=71)
    finalPublicationToken = models.CharField(max_length=32)
    temporaryPublicationToken = models.CharField(max_length=32)
    state = models.CharField(max_length=16, default="streaming")
    createdAt = models.DateTimeField(auto_now_add=True)
    updatedAt = models.DateTimeField(auto_now=True)
    finishedAt = models.DateTimeField(null=True, blank=True)

    class Meta:
        constraints = [models.CheckConstraint(condition=models.Q(state__in=["streaming", "staged", "committed", "abandoned"]),
            name="workspace_upload_state")]
        indexes = [models.Index(fields=["jobId", "authorizationDigest"], name="workspace_upload_job")]


class WorkspaceFilePack(models.Model):
    """One immutable physical file containing bounded, verified content ranges."""
    object = models.OneToOneField(WorkspaceSnapshotObject, primary_key=True,
        on_delete=models.CASCADE, related_name="filePack")
    sizeBytes = models.PositiveBigIntegerField()
    verifiedPublicationToken = models.CharField(max_length=32, blank=True, default="")


class WorkspaceFileObject(models.Model):
    """Verified content bytes in a Session-owned publication incarnation."""
    object = models.OneToOneField(WorkspaceSnapshotObject, primary_key=True,
        on_delete=models.CASCADE, related_name="fileContent")
    sha256 = models.CharField(max_length=71)
    sizeBytes = models.PositiveBigIntegerField()
    pack = models.ForeignKey(WorkspaceFilePack, null=True, blank=True,
        on_delete=models.PROTECT, related_name="files")
    packOffset = models.PositiveBigIntegerField(default=0)
    verifiedPublicationToken = models.CharField(max_length=32, blank=True, default="")


class WorkspaceFileReservation(models.Model):
    """A current lifecycle lease protects content until snapshot publication."""
    file = models.ForeignKey(WorkspaceFileObject, on_delete=models.CASCADE, related_name="reservations")
    agent_run = models.ForeignKey(AgentRun, on_delete=models.PROTECT, related_name="workspaceFileReservations")
    jobId = models.CharField(max_length=256)
    leaseOwner = models.CharField(max_length=160)
    authorizationDigest = models.CharField(max_length=71)
    publicationToken = models.CharField(max_length=32)

    class Meta:
        constraints = [models.UniqueConstraint(fields=["file", "jobId", "leaseOwner"],
            name="workspace_file_reservation_identity")]
        indexes = [models.Index(fields=["jobId", "authorizationDigest"], name="workspace_file_reservation_job")]


class WorkspaceSnapshotFileReference(models.Model):
    """Immutable manifest membership, indexed in both traversal directions."""
    snapshot = models.ForeignKey(WorkspaceSnapshotObject, on_delete=models.CASCADE, related_name="fileReferences")
    file = models.ForeignKey(WorkspaceFileObject, on_delete=models.PROTECT, related_name="snapshotReferences")

    class Meta:
        constraints = [models.UniqueConstraint(fields=["snapshot", "file"], name="workspace_snapshot_file_identity")]


class StorageCleanupRetry(models.Model):
    """Retry ownership for exact storage keys without a DerivedResource owner."""

    keyDigest = models.CharField(primary_key=True, max_length=64)
    storageKey = models.CharField(max_length=1000)
    collector = models.CharField(max_length=32)
    publicationToken = models.CharField(max_length=32, blank=True, default="")
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
        require_enum("StorageCleanupRetry.collector", self.collector, {"workspaceSnapshot", "orphanedLibrary", "retiredSession"})
        require_enum("StorageCleanupRetry.state", self.state, {"pending", "cleaning", "failed", "cleaned", "quarantined"})
        return super().save(*args, **kwargs)


class UploadCapacity(models.Model):
    """One database-wide counter for temporary bytes and active upload slots."""

    id = models.PositiveSmallIntegerField(primary_key=True, default=1)
    reservedBytes = models.PositiveBigIntegerField(default=0)
    activeUploads = models.PositiveIntegerField(default=0)


class UploadLease(models.Model):
    """A request liability, retained until its exact namespace is proven absent."""

    id = models.CharField(primary_key=True, max_length=32)
    poolRef = models.CharField(max_length=32)
    byteHold = models.PositiveBigIntegerField()
    uploadSlot = models.BooleanField()
    state = models.CharField(max_length=16, default="reserved")

    def save(self, *args, **kwargs):
        require_enum("UploadLease.state", self.state, {"reserved", "unknown"})
        return super().save(*args, **kwargs)


class WorkReturnAuditCursor(models.Model):
    id = models.CharField(primary_key=True, max_length=32)
    after = models.CharField(max_length=64, null=True)
    through = models.CharField(max_length=64, null=True)
    nextPageAtMs = models.BigIntegerField(default=0)
    leaseOwner = models.CharField(max_length=64, blank=True, default="")
    leaseExpiresAtMs = models.BigIntegerField(default=0)


class PresentationChange(models.Model):
    """Durable invalidation positions, never execution facts or delivered content."""
    key = models.CharField(primary_key=True, max_length=192)
    revision = models.PositiveBigIntegerField(default=0)


class PresentationChangeOutbox(models.Model):
    """One pending invalidation per source transaction and scoped reader key."""
    key = models.CharField(max_length=192)
    transactionId = models.PositiveBigIntegerField()

    class Meta:
        constraints = [models.UniqueConstraint(fields=["key", "transactionId"],
            name="presentation_change_transaction_unique")]


def new_issue_id() -> str:
    return new_id("issue")


def new_issue_event_id() -> str:
    return new_id("issue_event")


class Issue(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_issue_id)
    workspace = models.ForeignKey(Workspace, on_delete=models.CASCADE, related_name="issues")
    title = models.CharField(max_length=240)
    description = models.TextField(blank=True, default="")
    status = models.CharField(max_length=16, default="todo")
    assignee = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT,
        null=True, blank=True, related_name="assigned_issues")
    creator = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT, related_name="created_issues")
    version = models.PositiveIntegerField(default=1)
    archivedAt = models.DateTimeField(null=True, blank=True)
    createdAt = models.DateTimeField(auto_now_add=True)
    updatedAt = models.DateTimeField(auto_now=True)

    class Meta:
        indexes = [models.Index(fields=["workspace", "-updatedAt", "id"], name="issue_workspace_updated")]
        constraints = [
            models.CheckConstraint(condition=models.Q(status__in=["backlog", "todo", "inProgress", "done", "canceled"]), name="issue_status_valid"),
            models.CheckConstraint(condition=models.Q(version__gte=1), name="issue_version_positive"),
        ]


class IssueEvent(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_issue_event_id)
    issue = models.ForeignKey(Issue, on_delete=models.CASCADE, related_name="events")
    actor = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.PROTECT, related_name="issue_events")
    type = models.CharField(max_length=16)
    changes = models.JSONField(default=dict)
    createdAt = models.DateTimeField(auto_now_add=True)

    class Meta:
        indexes = [models.Index(fields=["issue", "createdAt", "id"], name="issue_event_chronological")]
        constraints = [models.CheckConstraint(condition=models.Q(type__in=["created", "updated", "archived", "restored"]), name="issue_event_type_valid")]



def new_notification_id() -> str:
    return new_id("notification")


class IssueNotification(models.Model):
    id = models.CharField(primary_key=True, max_length=64, default=new_notification_id)
    event = models.ForeignKey(IssueEvent, on_delete=models.CASCADE, related_name="notifications")
    recipient = models.ForeignKey(settings.AUTH_USER_MODEL, on_delete=models.CASCADE, related_name="issue_notifications")
    issueTitle = models.CharField(max_length=240)
    kind = models.CharField(max_length=16)
    readAt = models.DateTimeField(null=True, blank=True)
    archivedAt = models.DateTimeField(null=True, blank=True)
    createdAt = models.DateTimeField(auto_now_add=True)

    class Meta:
        indexes = [models.Index(fields=["recipient", "-createdAt", "-id"], name="notification_recipient_created")]
        constraints = [
            models.UniqueConstraint(fields=["event", "recipient"], name="notification_event_recipient_unique"),
            models.CheckConstraint(condition=models.Q(kind__in=["assigned", "statusChanged"]), name="notification_kind_valid"),
        ]
