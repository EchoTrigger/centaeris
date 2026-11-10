from ninja import Schema
from pydantic import ConfigDict
from pydantic.alias_generators import to_camel


class StrictSchema(Schema):
    model_config = ConfigDict(
        alias_generator=to_camel,
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
    is_staff: bool
    is_superuser: bool


class UserEnvelope(StrictSchema):
    user: UserResponse


class ModelResponse(StrictSchema):
    id: str
    display_name: str
    provider_id: str | None
    provider_display_name: str | None
    model_name: str
    context_tokens: int
    max_output_tokens: int
    thinking_mode: str | None
    thinking_modes: list[str]


class ModelsEnvelope(StrictSchema):
    models: list[ModelResponse]


class CsrfTokenResponse(StrictSchema):
    csrf_token: str


class LoginRequest(StrictSchema):
    email: str = ""
    password: str = ""


class PasswordChangeRequest(StrictSchema):
    current_password: str
    new_password: str


class PasswordResetRequest(StrictSchema):
    email: str


class PasswordResetConfirmRequest(StrictSchema):
    uid: str
    token: str
    new_password: str


class HealthResponse(StrictSchema):
    status: str
