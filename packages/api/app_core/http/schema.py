from ninja import Schema
from pydantic import ConfigDict, Field


class StrictSchema(Schema):
    model_config = ConfigDict(
        extra="forbid",
        from_attributes=True,
        strict=True,
        serialize_by_alias=True,
        validate_by_alias=True,
        validate_by_name=False,
    )


class ErrorResponse(StrictSchema):
    error: str


class OkResponse(StrictSchema):
    ok: bool


class UserResponse(StrictSchema):
    id: str
    email: str
    is_staff: bool = Field(alias="isStaff")
    is_superuser: bool = Field(alias="isSuperuser")


class UserEnvelope(StrictSchema):
    user: UserResponse


class ModelResponse(StrictSchema):
    id: str
    display_name: str = Field(alias="displayName")
    provider_id: str | None = Field(alias="providerId")
    provider_display_name: str | None = Field(alias="providerDisplayName")
    model_name: str = Field(alias="modelName")
    context_tokens: int = Field(alias="contextTokens")
    max_output_tokens: int = Field(alias="maxOutputTokens")
    thinking_mode: str | None = Field(alias="thinkingMode")
    thinking_modes: list[str] = Field(alias="thinkingModes")


class ModelsEnvelope(StrictSchema):
    models: list[ModelResponse]


class CsrfTokenResponse(StrictSchema):
    csrf_token: str = Field(alias="csrfToken")


class LoginRequest(StrictSchema):
    email: str = ""
    password: str = ""


class PasswordChangeRequest(StrictSchema):
    current_password: str = Field(alias="currentPassword")
    new_password: str = Field(alias="newPassword")


class PasswordResetRequest(StrictSchema):
    email: str


class PasswordResetConfirmRequest(StrictSchema):
    uid: str
    token: str
    new_password: str = Field(alias="newPassword")


class HealthResponse(StrictSchema):
    status: str
