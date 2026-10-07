import os

from django.core.asgi import get_asgi_application


os.environ.setdefault("DJANGO_SETTINGS_MODULE", "api.settings")

django_application = get_asgi_application()

from app_core.platform_mcp import create_mcp_app


class WorkspaceApplication:
    def __init__(self):
        self.mcp = create_mcp_app()

    async def __call__(self, scope, receive, send):
        if scope["type"] == "lifespan" or scope.get("path") == "/internal/mcp":
            return await self.mcp(scope, receive, send)
        return await django_application(scope, receive, send)


application = WorkspaceApplication()
