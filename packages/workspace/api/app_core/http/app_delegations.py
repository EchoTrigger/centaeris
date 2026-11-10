from typing import Literal

from django.db import transaction
from ninja import Router, Status
from pydantic import Field, field_validator

from app_core.credentials import validate_display_name
from app_core.models import BusinessApplication
from .response_schema import COMMON_ERROR_RESPONSES
from .schema import StrictSchema
from .security import session_auth, superuser_auth


router = Router(tags=["app-delegations"], by_alias=True)


class BusinessAppResponse(StrictSchema):
    id: str
    name: str
    status: Literal["pending", "active", "revoked"]


class BusinessAppEnvelope(StrictSchema):
    app: BusinessAppResponse


class BusinessAppsEnvelope(StrictSchema):
    apps: list[BusinessAppResponse]


class RegisterAppRequest(StrictSchema):
    name: str = Field(min_length=1, max_length=160)

    @field_validator("name")
    @classmethod
    def valid_name(cls, value):
        validate_display_name(value)
        return value


class AppStatusRequest(StrictSchema):
    status: Literal["active", "revoked"]


def serialize_app(app):
    return {"id": app.id, "name": app.name, "status": app.status}


@router.get("/business-apps", auth=session_auth, response={200: BusinessAppsEnvelope} | COMMON_ERROR_RESPONSES)
def list_business_apps(request):
    return {"apps": [serialize_app(app) for app in BusinessApplication.objects.filter(status="active").order_by("name", "id")]}


@router.get("/admin/business-apps", auth=superuser_auth, response={200: BusinessAppsEnvelope} | COMMON_ERROR_RESPONSES)
def list_registered_business_apps(request):
    return {"apps": [serialize_app(app) for app in BusinessApplication.objects.all().order_by("created_at", "id")]}


@router.post("/admin/business-apps", auth=superuser_auth, response={201: BusinessAppEnvelope} | COMMON_ERROR_RESPONSES)
def register_business_app(request, payload: RegisterAppRequest):
    app = BusinessApplication.objects.create(name=payload.name, created_by=request.user)
    return Status(201, {"app": serialize_app(app)})


@router.patch("/admin/business-apps/{app_id}", auth=superuser_auth, response={200: BusinessAppEnvelope} | COMMON_ERROR_RESPONSES)
def update_business_app(request, app_id: str, payload: AppStatusRequest):
    with transaction.atomic():
        app = BusinessApplication.objects.select_for_update().filter(id=app_id).first()
        if app is None:
            return Status(404, {"error": "business_app_not_found"})
        if app.status == "revoked":
            return Status(409, {"error": "business_app_revoked"})
        app.status = payload.status
        app.save(update_fields=["status", "updated_at"])
    return {"app": serialize_app(app)}
