from django.conf import settings
from app_core.http.office_preview import router as office_preview_router
from ninja import NinjaAPI

from app_core.http.app_delegations import router as business_apps_router
from app_core.http.auth import router as auth_router
from app_core.http.sso import router as sso_router
from app_core.http.assistant_connectors import internal_router as internal_assistant_connectors_router
from app_core.http.artifacts import router as artifacts_router
from app_core.http.citations import router as citations_router
from app_core.http.downloads import router as downloads_router
from app_core.http.errors import install_error_handlers
from app_core.http.health import router as health_router
from app_core.http.environment_profiles import router as environment_profiles_router
from app_core.http.internal import router as internal_router
from app_core.http.material_inputs import router as material_inputs_router
from app_core.http.internal_model import router as internal_model_router
from app_core.http.jobs import router as jobs_router
from app_core.http.library import router as library_router
from app_core.http.sources import router as sources_router
from app_core.http.streaming import router as streaming_router
from app_core.http.presentation import router as presentation_router
from app_core.http.trash import router as trash_router
from app_core.http.workspace_members import router as workspace_members_router
from app_core.http.workspaces import router as workspaces_router
from app_core.http.session_subagents import router as session_subagents_router
from app_core.http.workspace_profile import router as workspace_profile_router
from app_core.http.workspace_resources import plugin_upload_router
from app_core.http.environment_settings import router as environment_settings_router
from app_core.http.environment_maintenance import router as environment_maintenance_router


api = NinjaAPI(
    title="Centaeris Workspace API",
    version="0.1.0",
    docs_url="/docs" if settings.DEBUG else None,
    openapi_url="/openapi.json" if settings.DEBUG else None,
    urls_namespace="workspace_agent_api",
)
install_error_handlers(api)
api.add_router("", health_router)
api.add_router("/api", auth_router)
api.add_router("/api", business_apps_router)
api.add_router("/api", sso_router)
api.add_router("/api", jobs_router)
api.add_router("/api", plugin_upload_router)
api.add_router("/api", workspaces_router)
api.add_router("/api", session_subagents_router)
api.add_router("/api", workspace_profile_router)
api.add_router("/api", environment_settings_router)
api.add_router("/api", workspace_members_router)
api.add_router("/api", citations_router)
api.add_router("/api", sources_router)
api.add_router("/api", library_router)
api.add_router("/api", artifacts_router)
api.add_router("/api", downloads_router)
api.add_router("/api", office_preview_router)
api.add_router("/api", streaming_router)
api.add_router("/api", presentation_router)
api.add_router("/api", trash_router)
api.add_router("/internal", internal_router)
api.add_router("/internal", material_inputs_router)
api.add_router("/internal", environment_profiles_router)
api.add_router("/internal", environment_maintenance_router)
api.add_router("/internal", internal_model_router)
api.add_router("/internal", internal_assistant_connectors_router)

from app_core.http.workspace_resources import router as workspace_resources_router
api.add_router("/api", workspace_resources_router)

from app_core.http.agent_run_api import router as agent_run_api_router
api.add_router("/api", agent_run_api_router)

from app_core.http.issues import router as issues_router
api.add_router('/api', issues_router)

from app_core.http.notifications import router as notifications_router
api.add_router("/api", notifications_router)

from app_core.http.workspace_guide import router as workspace_guide_router
api.add_router("/api", workspace_guide_router)

from app_core.http.issue_executions import router as issue_executions_router
api.add_router("/api", issue_executions_router)

from app_core.http.model_defaults import router as model_defaults_router
api.add_router("/api", model_defaults_router)
