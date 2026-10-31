import os
import django


os.environ.setdefault("DJANGO_SETTINGS_MODULE", "api.settings")

django.setup(set_prefix=False)

from app_core.api_memory import ApiMemoryObservations, mark_response_stream
from app_core.platform_mcp import create_mcp_app
from app_core.presentation_service import create_presentation_hub
from app_core.upload_ingress import ManagedUploadASGIHandler, StorageIngressApplication

class ObservedUploadASGIHandler(ManagedUploadASGIHandler):
    async def send_response(self, response, send):
        if response.streaming:
            mark_response_stream()
        return await super().send_response(response, send)


django_application = ObservedUploadASGIHandler()


class WorkspaceApplication:
    def __init__(self):
        self.mcp = create_mcp_app()
        self.presentation_hub = create_presentation_hub()
        self.memory_observations = ApiMemoryObservations()
        self.http_application = StorageIngressApplication(self._serve_http)

    async def __call__(self, scope, receive, send):
        if scope["type"] == "lifespan":
            async def owned_receive():
                message = await receive()
                if message["type"] == "lifespan.shutdown":
                    await self.memory_observations.aclose()
                    await self.presentation_hub.aclose()
                return message
            async def owned_send(message):
                if message["type"] == "lifespan.startup.complete":
                    self.memory_observations.start()
                await send(message)
            try:
                return await self.mcp(scope, owned_receive, owned_send)
            finally:
                await self.memory_observations.aclose()
        if scope["type"] == "http":
            return await self.memory_observations.observe_http(scope, receive, send, self.http_application)
        return await self._serve_http(scope, receive, send)

    async def _serve_http(self, scope, receive, send):
        owned_scope = {**scope, "centaeris.presentationHub": self.presentation_hub}
        if scope.get("path") == "/internal/mcp":
            return await self.mcp(owned_scope, receive, send)
        return await django_application(owned_scope, receive, send)


application = WorkspaceApplication()
