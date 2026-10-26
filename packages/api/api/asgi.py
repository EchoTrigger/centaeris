import os
import django


os.environ.setdefault("DJANGO_SETTINGS_MODULE", "api.settings")

django.setup(set_prefix=False)

from app_core.platform_mcp import create_mcp_app
from app_core.presentation_service import create_presentation_hub
from app_core.upload_ingress import ManagedUploadASGIHandler, StorageIngressApplication

django_application = ManagedUploadASGIHandler()


class WorkspaceApplication:
    def __init__(self):
        self.mcp = create_mcp_app()
        self.presentation_hub = create_presentation_hub()

    async def __call__(self, scope, receive, send):
        if scope["type"] == "lifespan":
            async def owned_receive():
                message = await receive()
                if message["type"] == "lifespan.shutdown":
                    await self.presentation_hub.aclose()
                return message
            return await self.mcp(scope, owned_receive, send)
        owned_scope = {**scope, "centaeris.presentationHub": self.presentation_hub}
        if scope.get("path") == "/internal/mcp":
            return await self.mcp(owned_scope, receive, send)
        return await django_application(owned_scope, receive, send)


application = StorageIngressApplication(WorkspaceApplication())
