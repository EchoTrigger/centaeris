import logging
from app_core.app_delegations import DelegationRejected
from app_core.business_agent_scope import BusinessAgentConfigurationUnavailable

from ninja.errors import HttpError, ValidationError

from .security import (
    InternalAuthenticationRequired,
    PublicAuthenticationRequired,
    PublicCsrfRejected,
    SuperuserRequired,
)


logger = logging.getLogger(__name__)

VALIDATION_ERROR_CODES = {
    "register_business_app": "business_app_request_invalid",
    "update_business_app": "business_app_request_invalid",
    "issue_app_delegation": "delegation_request_invalid",
    "rotate_app_delegation": "delegation_request_invalid",
    "resolve_business_agent_branch": "business_branch_request_invalid",
    "approve_assistant_credential": "mcp_connector_request_invalid",
    "bind_assistant_connector": "mcp_connector_request_invalid",
    "create_agent_definition": "agent_definition_invalid",
    "update_agent_definition": "agent_definition_invalid",
    "publish_agent_definition": "agent_definition_invalid",
    "set_agent_definition_availability": "agent_definition_invalid",
    "use_agent_definition": "agent_definition_invalid",
    "change_password": "account_password_request_invalid",
    "request_password_reset": "account_password_reset_request_invalid",
    "confirm_password_reset": "account_password_reset_request_invalid",
    "create_agent": "agent_invalid",
    "update_agent": "agent_invalid",
    "create_source": "invalid_source",
    "update_source": "invalid_source",
    "create_source_grant": "invalid_source_grant",
    "update_source_grant": "invalid_source_grant",
    "create_credential": "credential_invalid",
    "rotate_credential": "credential_invalid",
    "create_model": "model_config_invalid",
    "update_model": "model_config_invalid",
    "test_model": "invalid_json",
    "create_library_folder": "library_folder_request_invalid",
    "create_library_note": "library_note_request_invalid",
    "move_library_object": "library_move_request_invalid",
    "update_library_note": "library_note_request_invalid",
    "attach_session_asset": "asset_not_accessible",
    "detach_session_asset": "asset_link_not_found",
    "create_workspace_invitation": "workspace_invitation_invalid",
    "preview_workspace_invitation": "workspace_invitation_preview_invalid",
    "accept_workspace_invitation": "workspace_invitation_accept_invalid",
    "update_workspace_member_role": "workspace_member_role_invalid",
    "transfer_workspace_ownership": "workspace_owner_transfer_invalid",
    "create_workspace_group": "workspace_group_invalid",
    "update_workspace_group": "workspace_group_invalid",
}

# Django Ninja combines operations that share a path into one Django URL pattern.
# Django's resolver therefore exposes the first operation's url_name for every
# method on that path. Keep the HTTP-method disambiguation explicit so validation
# errors retain the public protocol code owned by the selected operation.
VALIDATION_ERROR_CODE_OVERRIDES = {
    ("POST", "list_registered_business_apps"): "business_app_request_invalid",
    ("POST", "list_app_delegations"): "delegation_request_invalid",
    ("POST", "list_agent_definitions"): "agent_definition_invalid",
    ("PATCH", "get_agent_definition"): "agent_definition_invalid",
    ("POST", "list_agent_definition_versions"): "agent_definition_invalid",
    ("POST", "list_agents"): "agent_invalid",
    ("POST", "list_sources"): "invalid_source",
    ("POST", "list_credentials"): "credential_invalid",
    ("POST", "list_models"): "model_config_invalid",
    ("PATCH", "get_library_object"): "library_move_request_invalid",
    ("PUT", "get_library_note"): "library_note_request_invalid",
    ("POST", "list_session_assets"): "asset_not_accessible",
    ("DELETE", "list_session_assets"): "asset_link_not_found",
    ("POST", "list_workspace_invitations"): "workspace_invitation_invalid",
    ("POST", "list_workspace_groups"): "workspace_group_invalid",
    ("POST", "list_source_grants"): "invalid_source_grant",
}

MALFORMED_JSON_ERROR_CODES = {
    "register_business_app": "business_app_request_invalid",
    "update_business_app": "business_app_request_invalid",
    "issue_app_delegation": "delegation_request_invalid",
    "rotate_app_delegation": "delegation_request_invalid",
    "resolve_business_agent_branch": "business_branch_request_invalid",
    "approve_assistant_credential": "mcp_connector_request_invalid",
    "bind_assistant_connector": "mcp_connector_request_invalid",
    "create_agent_definition": "agent_definition_invalid",
    "update_agent_definition": "agent_definition_invalid",
    "publish_agent_definition": "agent_definition_invalid",
    "set_agent_definition_availability": "agent_definition_invalid",
    "use_agent_definition": "agent_definition_invalid",
    "change_password": "account_password_request_invalid",
    "request_password_reset": "account_password_reset_request_invalid",
    "confirm_password_reset": "account_password_reset_request_invalid",
    "create_agent": "agent_invalid",
    "update_agent": "agent_invalid",
    "create_source": "invalid_source",
    "update_source": "invalid_source",
    "create_source_grant": "invalid_source_grant",
    "update_source_grant": "invalid_source_grant",
    "create_credential": "credential_invalid",
    "rotate_credential": "credential_invalid",
    "create_model": "model_config_invalid",
    "update_model": "model_config_invalid",
    "create_library_folder": "library_folder_request_invalid",
    "create_library_note": "library_note_request_invalid",
    "move_library_object": "library_move_request_invalid",
    "update_library_note": "library_note_request_invalid",
    "attach_session_asset": "asset_not_accessible",
    "detach_session_asset": "asset_link_not_found",
    "create_workspace_invitation": "workspace_invitation_invalid",
    "preview_workspace_invitation": "workspace_invitation_preview_invalid",
    "accept_workspace_invitation": "workspace_invitation_accept_invalid",
    "update_workspace_member_role": "workspace_member_role_invalid",
    "transfer_workspace_ownership": "workspace_owner_transfer_invalid",
    "create_workspace_group": "workspace_group_invalid",
    "update_workspace_group": "workspace_group_invalid",
}

MALFORMED_JSON_ERROR_CODE_OVERRIDES = {
    key: value
    for key, value in VALIDATION_ERROR_CODE_OVERRIDES.items()
    if key[0] != "PATCH" or key[1] != "get_session"
}

VALIDATION_ERROR_STATUSES = {
    "asset_not_accessible": 403,
    "asset_link_not_found": 404,
}


def _resolved_operation(request) -> tuple[str, str]:
    operation_name = request.resolver_match.url_name if request.resolver_match else ""
    return request.method, operation_name


def _mapped_error_code(request, *, malformed_json: bool) -> str:
    operation_key = _resolved_operation(request)
    operation_name = operation_key[1]
    if malformed_json:
        return MALFORMED_JSON_ERROR_CODE_OVERRIDES.get(
            operation_key,
            MALFORMED_JSON_ERROR_CODES.get(operation_name, "invalid_json"),
        )
    return VALIDATION_ERROR_CODE_OVERRIDES.get(
        operation_key,
        VALIDATION_ERROR_CODES.get(operation_name, "request_invalid"),
    )


def install_error_handlers(api) -> None:
    @api.exception_handler(BusinessAgentConfigurationUnavailable)
    def business_agent_configuration_unavailable(request, error):
        return api.create_response(request, {"error": str(error)}, status=409)

    @api.exception_handler(DelegationRejected)
    def delegation_rejected(request, error):
        return api.create_response(request, {"error": error.code}, status=error.status)

    @api.exception_handler(PublicAuthenticationRequired)
    def public_authentication_required(request, error):
        return api.create_response(
            request,
            {"error": "authentication_required"},
            status=401,
        )

    @api.exception_handler(InternalAuthenticationRequired)
    def internal_authentication_required(request, error):
        return api.create_response(request, {"error": "unauthorized"}, status=401)

    @api.exception_handler(PublicCsrfRejected)
    def public_csrf_rejected(request, error):
        return api.create_response(request, {"error": "csrf_failed"}, status=403)

    @api.exception_handler(SuperuserRequired)
    def superuser_required(request, error):
        return api.create_response(
            request,
            {"error": "superuser_required"},
            status=403,
        )

    @api.exception_handler(ValidationError)
    def validation_error(request, error):
        operation_name = _resolved_operation(request)[1]
        error_code = _mapped_error_code(
            request,
            malformed_json=any(
                item.get("type") == "json_invalid" for item in error.errors
            ),
        )
        if operation_name == "create_session_message" and any(
            "attachmentRefs" in item.get("loc", ()) for item in error.errors
        ):
            error_code = "invalid_attachment_refs"
        fields = sorted(
            {
                ".".join(str(part) for part in item.get("loc", ()) if part != "body")
                for item in error.errors
            }
            - {""}
        )
        body = {"error": error_code}
        if fields and error_code == "request_invalid":
            body["fields"] = fields
        return api.create_response(
            request,
            body,
            status=VALIDATION_ERROR_STATUSES.get(error_code, 400),
        )

    @api.exception_handler(HttpError)
    def http_error(request, error):
        if error.status_code == 403 and error.message == "CSRF check Failed":
            body = {"error": "csrf_failed"}
        elif (
            error.status_code == 400
            and error.message.startswith("Cannot parse request body")
        ):
            error_code = _mapped_error_code(request, malformed_json=True)
            body = {"error": error_code}
        else:
            body = {"error": "http_error"}
        return api.create_response(
            request,
            body,
            status=VALIDATION_ERROR_STATUSES.get(
                body["error"],
                error.status_code,
            ),
        )

    @api.exception_handler(Exception)
    def unexpected_error(request, error):
        logger.error(
            "Unhandled API operation failure (%s)",
            type(error).__name__,
            extra={
                "requestPath": request.path,
                "exceptionType": type(error).__name__,
            },
        )
        return api.create_response(
            request,
            {"error": "internal_error"},
            status=500,
        )
