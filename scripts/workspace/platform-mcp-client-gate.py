"""Real Rust ToolLayer -> MCP HTTP -> Django receipts interoperability gate.

Owns a random test database and a loopback listener. Seeds a processed material;
does not claim Docker processor, model loop, or lease-recovery acceptance.
"""

import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import time
import uuid


def main():
    root = Path(__file__).resolve().parents[1]
    sys.path.insert(0, str(root / "packages/api"))
    with tempfile.TemporaryDirectory(prefix="platform-mcp-client-") as storage:
        os.environ.update(DJANGO_SETTINGS_MODULE="api.test_settings", STORAGE_ROOT=storage,
                          RUNTIME_URL="http://127.0.0.1:1", REDIS_URL="redis://127.0.0.1:1/15")
        import django
        django.setup()
        import uvicorn
        from django.conf import settings
        from django.contrib.auth.models import User
        from django.core.files.base import ContentFile
        from django.core.files.storage import default_storage
        from django.db import connections
        from django.test import Client
        from django.test.runner import DiscoverRunner
        from api.asgi import WorkspaceApplication
        from app_core.models import (Workspace, WorkspaceMembership, ModelConfig, AgentRun, UserLibraryObject,
                                     SessionAssetLink, DerivedRepresentation, ProcessingSpecification, MaterialEvidenceReceipt)
        from app_core.testing import create_session
        from app_core.assets import captured_input_fields
        from app_core.agent_run_authorization_factory import create_agent_run_authorization
        from app_core.material_contract import sha256_bytes
        from app_core.material_identity import processing_spec_digest, representation_id
        from app_core.test_platform_mcp import processing_specification
        from app_core.session_event import rebuild_agent_run_citation_projection

        settings.DATABASES["default"]["TEST"]["NAME"] = "test_platform_mcp_" + uuid.uuid4().hex[:12]
        runner = DiscoverRunner(interactive=False, verbosity=0)
        databases = runner.setup_databases()
        server = thread = listener = None
        try:
            user = User.objects.create_user(username="material-client")
            workspace = Workspace.objects.create(name="Client test", createdBy=user)
            WorkspaceMembership.objects.create(workspace=workspace, user=user, role="owner")
            session = create_session(workspace=workspace, owner=user)
            model = ModelConfig.objects.create(id="material-model", displayName="Test")
            content = (("Real Rust client material evidence 界😀. " * 6000) + "\n").encode("utf-8")
            key = default_storage.save("test/material.md", ContentFile(content))
            item = UserLibraryObject.objects.create(owner=user, displayName="Material.md", objectKind="file",
                contentType="text/markdown", sizeBytes=len(content), sha256=sha256_bytes(content), storageKey=key,
                contentGeneration=1, status="ready")
            link = SessionAssetLink.objects.create(workspace=workspace, session=session, userLibraryObject=item,
                attachedBy=user, capturedDisplayName=item.displayName, capturedContentType=item.contentType,
                **captured_input_fields(item))
            run = AgentRun.objects.create(workspace=workspace, session=session, user=user, modelConfig=model, prompt="Read material")
            authorization = create_agent_run_authorization(run, [link.id], image_digest="sha256:" + "a" * 64)
            specification = processing_specification()
            digest = processing_spec_digest(specification)
            spec = ProcessingSpecification.objects.create(specDigest=digest, payload=specification)
            identity = authorization.payload["assetRefs"][0]["inputIdentity"]
            DerivedRepresentation.objects.create(representationId=representation_id(identity, digest),
                ownerKind="userLibraryObject", ownerId=item.id, ownerContentGeneration=1, ownerSha256=item.sha256,
                processingSpecification=spec, pageCount=1, canonicalTextKey=key, canonicalTextSizeBytes=len(content),
                canonicalTextSha256=item.sha256, manifest={"pages": [{"pageText": {"page": 1, "route": "nativeText"},
                    "canonicalStartByte": 0, "canonicalEndByte": len(content)}]})
            requests = []
            application = WorkspaceApplication()

            async def counted(scope, receive, send):
                if scope["type"] == "http":
                    requests.append(scope["path"])
                await application(scope, receive, send)

            listener = socket.socket()
            listener.bind(("127.0.0.1", 0))
            listener.listen()
            address = f"http://127.0.0.1:{listener.getsockname()[1]}"
            server = uvicorn.Server(uvicorn.Config(counted, log_level="error", lifespan="on"))
            thread = threading.Thread(target=server.run, kwargs={"sockets": [listener]}, daemon=True)
            thread.start()
            deadline = time.monotonic() + 10
            while not server.started and thread.is_alive() and time.monotonic() < deadline:
                time.sleep(0.02)
            if not server.started:
                raise RuntimeError("test API failed to start")
            database = settings.DATABASES["default"]
            fixture = {"apiUrl": address, "token": settings.INTERNAL_API_TOKEN, "runId": run.id,
                "sessionId": session.id, "workspaceId": workspace.id, "turnId": run.turn_id,
                "authorizationDigest": authorization.digest, "specification": specification,
                "specDigest": digest, "inputRef": link.id, "expectedTextSha256": sha256_bytes(content),
                "database": {key: database[key] for key in ("HOST", "PORT", "NAME", "USER", "PASSWORD")}}
            environment = {**os.environ, "PLATFORM_MCP_CLIENT_FIXTURE": json.dumps(fixture)}
            subprocess.run(["cargo", "test", "--locked", "-p", "runtime_server",
                "platform_materials::tests::python_http_interoperability", "--", "--ignored", "--exact", "--nocapture"],
                cwd=root, env=environment, check=True, timeout=180)
            count = MaterialEvidenceReceipt.objects.count()
            assert requests.count("/internal/mcp/credential") == count + 2, requests
            assert count > 1
            projected = rebuild_agent_run_citation_projection(run)
            assert len(projected) == count
            assert len(rebuild_agent_run_citation_projection(run)) == count
            browser = Client(HTTP_HOST="localhost")
            browser.force_login(user)
            snapshot_response = browser.get(f"/api/sessions/{session.id}/agent-runs/{run.id}/citations")
            assert snapshot_response.status_code == 200
            snapshot = snapshot_response.json()
            assert len(snapshot["citations"]) == count
            # Cross-language parity: the real Python response must pass the web validator.
            subprocess.run(["node", "--input-type=module", "-e",
                "import {readFileSync} from 'node:fs'; import {validateCitationSnapshot} from './packages/web/src/chat/citationSnapshot.ts'; "
                "const s=JSON.parse(readFileSync(0,'utf8')); validateCitationSnapshot(s,s.sessionId,s.agentRunId);"],
                cwd=root, input=json.dumps(snapshot), text=True, check=True, timeout=30)
            preview = browser.get(f"/api/citations/{projected[0].citationId}/preview")
            assert preview.status_code == 200, (preview.status_code, preview.content)
            from app_core.tests import streaming_response_bytes
            assert streaming_response_bytes(preview) == content
            print("Platform MCP client gate: real Rust calls, credential refresh, durable receipts and preview passed")
        finally:
            if server is not None:
                server.should_exit = True
            if thread is not None:
                thread.join(timeout=10)
            if listener is not None:
                listener.close()
            connections.close_all()
            runner.teardown_databases(databases)


if __name__ == "__main__":
    main()
