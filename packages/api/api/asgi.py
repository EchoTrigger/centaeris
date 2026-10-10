import os
import django


os.environ.setdefault("DJANGO_SETTINGS_MODULE", "api.settings")

django.setup(set_prefix=False)

from app_core.platform_mcp import create_mcp_app
from app_core.upload_ingress import ManagedUploadASGIHandler, StorageIngressApplication

django_application = ManagedUploadASGIHandler()


class WorkspaceApplication:
    def __init__(self):
        self.mcp = create_mcp_app()

    async def __call__(self, scope, receive, send):
        if scope["type"] == "lifespan" or scope.get("path") == "/internal/mcp":
            return await self.mcp(scope, receive, send)
        return await django_application(scope, receive, send)


application = StorageIngressApplication(WorkspaceApplication())
