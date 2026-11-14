from django.db import transaction
from ninja import Router, Status
from pydantic import Field

from app_core.hosted_operations import HostedOperationError
from app_core.issue_templates import validate_definition
from app_core.models import WorkspaceIssueTemplate
from app_core.workspace_access import WORKSPACE_ADMIN_ROLES, locked_workspace_membership_for, workspace_membership_for
from .schema import StrictSchema
from .security import session_auth
from .response_schema import COMMON_ERROR_RESPONSES

router = Router(tags=["issue-templates"], by_alias=True)


class TemplateResponse(StrictSchema):
    key: str
    name: str
    revision: int
    definition: dict


class TemplatesEnvelope(StrictSchema):
    templates: list[TemplateResponse]


class TemplateRequest(StrictSchema):
    key: str = Field(pattern=r"^[a-z][a-z0-9-]{0,63}$")
    name: str = Field(min_length=1, max_length=160)
    definition: dict


@router.get("/workspaces/{workspace_id}/issue-templates", auth=session_auth,
            response={200: TemplatesEnvelope} | COMMON_ERROR_RESPONSES)
def list_templates(request, workspace_id: str):
    if workspace_membership_for(request.user, workspace_id) is None:
        return Status(404, {"error": "workspace_not_found"})
    return {"templates": [{"key": t.key, "name": t.name, "revision": t.revision, "definition": t.definition}
                          for t in WorkspaceIssueTemplate.objects.filter(workspace_id=workspace_id, enabled=True).order_by("name", "key")]}


@router.post("/workspaces/{workspace_id}/issue-templates", auth=session_auth,
             response={201: TemplateResponse} | COMMON_ERROR_RESPONSES)
def install_template(request, workspace_id: str, payload: TemplateRequest):
    if workspace_membership_for(request.user, workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES) is None:
        return Status(404, {"error": "workspace_not_found"})
    try:
        definition = validate_definition(payload.definition).model_dump(by_alias=True)
    except HostedOperationError as error:
        return Status(error.status, {"error": error.code})
    with transaction.atomic():
        if locked_workspace_membership_for(request.user, workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES) is None:
            return Status(404, {"error": "workspace_not_found"})
        if WorkspaceIssueTemplate.objects.filter(workspace_id=workspace_id, key=payload.key).exists():
            return Status(409, {"error": "issue_template_exists"})
        item = WorkspaceIssueTemplate.objects.create(workspace_id=workspace_id, key=payload.key, name=payload.name, definition=definition)
    return Status(201, {"key": item.key, "name": item.name, "revision": item.revision, "definition": item.definition})
