"""Synthetic processor acceptance in the owned isolated stack; no model calls."""
import json
import os
from pathlib import Path
import sys
import time
import uuid
import re

if os.environ.get("MCP_E2E_ISOLATED") != "1":
    raise RuntimeError("Requires isolated acceptance deployment")
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "packages/api"))
os.environ.setdefault("DJANGO_SETTINGS_MODULE", "api.settings")
import django
django.setup()

from django.contrib.auth.models import User
from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from app_core.models import (Workspace, WorkspaceMembership, ModelConfig, AgentRun, UserLibraryObject,
    SessionAssetLink, MaterialProcessor, MaterialProcessingTask, DerivedRepresentation)
from app_core.testing import create_session
from app_core.assets import captured_input_fields
from app_core.agent_run_authorization_factory import create_agent_run_authorization
from app_core.material_access import MaterialAccessContext
from app_core.platform_mcp_auth import authorize_context
from app_core.material_task_source import admit_task
from app_core.material_identity import representation_id
from app_core.material_contract import sha256_bytes
from app_core.material_operations import create_operations, operation_result


def verify(task_id):
    task = MaterialProcessingTask.objects.get(pk=task_id)
    assert task.status == "completed", task.status
    owner = UserLibraryObject.objects.get(pk=task.payload["inputIdentity"]["ownerId"])
    result = DerivedRepresentation.objects.get(pk=task.pk)
    with default_storage.open(owner.storageKey) as original, default_storage.open(result.canonicalTextKey) as stored:
        nonce = re.search(rb"Material acceptance nonce ([0-9a-f]{32})", original.read()).group(1)
        actual = stored.read()
    assert nonce in actual
    assert sha256_bytes(actual) == result.canonicalTextSha256
    for operation in task.operations.select_related("agent_run__authorization"):
        operation_access = authorize_context(MaterialAccessContext(
            operation.agent_run_id, operation.agent_run.authorization.digest,
            task.processingSpecification, task.payload["specDigest"]))
        expected = "cancelled" if operation.cancelledAt is not None else "completed"
        assert operation_result(operation_access, operation.pk)["status"] == expected
    print(json.dumps({"stage": "verified", "task": task.pk, "attempts": task.attemptCount, "pageCount": result.pageCount}), flush=True)


if len(sys.argv) == 3 and sys.argv[1] == "--verify":
    verify(sys.argv[2])
    raise SystemExit(0)


def pdf_document(text, pages=100):
    objects = [b"<< /Type /Catalog /Pages 2 0 R >>", b"", b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"]
    kids = []
    for index in range(pages):
        page = len(objects) + 1
        kids.append(f"{page} 0 R")
        objects.append(f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {page+1} 0 R >>".encode())
        data = b"BT /F1 12 Tf 40 740 Td (" + text + b") Tj ET"
        objects.append(f"<< /Length {len(data)} >>\nstream\n".encode() + data + b"\nendstream")
    objects[1] = f"<< /Type /Pages /Count {pages} /Kids [{' '.join(kids)}] >>".encode()
    output, offsets = bytearray(b"%PDF-1.4\n"), [0]
    for index, obj in enumerate(objects, 1):
        offsets.append(len(output))
        output.extend(f"{index} 0 obj\n".encode() + obj + b"\nendobj\n")
    xref = len(output)
    output.extend(f"xref\n0 {len(offsets)}\n0000000000 65535 f \n".encode())
    for offset in offsets[1:]:
        output.extend(f"{offset:010} 00000 n \n".encode())
    output.extend(f"trailer << /Size {len(offsets)} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode())
    return bytes(output)

user = User.objects.create_user(username="processor-gate-" + uuid.uuid4().hex)
workspace = Workspace.objects.create(name="Processor acceptance", createdBy=user)
WorkspaceMembership.objects.create(workspace=workspace, user=user, role="owner")
session = create_session(workspace=workspace, owner=user)
model = ModelConfig.objects.create(id="processor-gate-" + uuid.uuid4().hex, displayName="Synthetic")
content = ("Material acceptance nonce " + uuid.uuid4().hex).encode()
is_pdf = "--pdf" in sys.argv
if is_pdf:
    content = pdf_document(content)
key = default_storage.save("processor-acceptance/" + uuid.uuid4().hex + ".txt", ContentFile(content))
material = UserLibraryObject.objects.create(owner=user, objectKind="file", displayName="acceptance.pdf" if is_pdf else "acceptance.txt", contentType="application/pdf" if is_pdf else "text/plain",
    sizeBytes=len(content), sha256=sha256_bytes(content), storageKey=key, contentGeneration=1, status="ready")
link = SessionAssetLink.objects.create(workspace=workspace, session=session, userLibraryObject=material, attachedBy=user,
    capturedDisplayName=material.displayName, capturedContentType=material.contentType, **captured_input_fields(material))
run = AgentRun.objects.create(workspace=workspace, session=session, user=user, modelConfig=model, prompt="processor acceptance")
auth = create_agent_run_authorization(run, image_digest="sha256:" + "a" * 64)
processor = MaterialProcessor.objects.select_related("specification").get(name="document")
spec, digest = processor.specification.payload, processor.specification_id
access = authorize_context(MaterialAccessContext(run.pk, auth.digest, spec, digest))
identity = auth.payload["assetRefs"][0]["inputIdentity"]
task = admit_task(access, {"inputRef": link.pk, "representationId": representation_id(identity, digest)})
if "--cancel-waiter" in sys.argv or "--cancel-all" in sys.argv:
    binding = {"inputRef": link.pk, "representationId": task.pk}
    first = create_operations(access, [binding])[0]
    second_run = AgentRun.objects.create(workspace=workspace, session=session, user=user,
                                        modelConfig=model, prompt="second waiter")
    second_auth = create_agent_run_authorization(second_run, image_digest="sha256:" + "a" * 64)
    second_access = authorize_context(MaterialAccessContext(second_run.pk, second_auth.digest, spec, digest))
    second = create_operations(second_access, [binding])[0]
    assert operation_result(access, first["operationId"], cancel=True)["status"] == "cancelled"
    if "--cancel-all" in sys.argv:
        assert operation_result(second_access, second["operationId"], cancel=True)["status"] == "cancelled"
print(json.dumps({"stage": "admitted", "task": task.pk, "run": run.pk}), flush=True)
if "--seed-only" in sys.argv:
    raise SystemExit(0)
deadline = time.monotonic() + 180
while time.monotonic() < deadline:
    task.refresh_from_db()
    if task.status == "completed":
        result = DerivedRepresentation.objects.get(pk=task.pk)
        with default_storage.open(result.canonicalTextKey) as stored:
            actual = stored.read()
        assert re.search(rb"Material acceptance nonce ([0-9a-f]{32})", content).group(1) in actual
        print(json.dumps({"stage": "processed", "task": task.pk, "attempts": task.attemptCount,
            "canonicalSha256": result.canonicalTextSha256, "bytes": len(actual)}), flush=True)
        verify(task.pk)
        break
    if task.status == "failed":
        raise RuntimeError(task.errorCode)
    time.sleep(1)
else:
    raise RuntimeError("processor acceptance timeout")
