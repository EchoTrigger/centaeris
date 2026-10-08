"""Hosted app delegation identities and operation scopes; no caller-owned claims."""

ISSUER = "centaeris-workspace"
AUDIENCE = "centaeris-workspace-api"
SCOPES = frozenset({"assistant:use", "sessions:read", "sessions:create", "messages:submit",
                    "attachments:write", "events:read", "artifacts:read", "runs:cancel"})

# Native Agents accept inputs into the existing coordinator executed by its owner.
NATIVE_SCOPES = frozenset({"assistant:use", "messages:submit", "sessions:read", "artifacts:read", "attachments:write"})


def validate_scopes(value):
    if (not isinstance(value, list) or not value
            or any(not isinstance(scope, str) or scope not in SCOPES for scope in value)
            or len(set(value)) != len(value)):
        raise ValueError("delegation_request_invalid")
    return sorted(value)
