"""Opt-in live model acceptance, inside the isolated MCP E2E API container.

Input is one stdin JSON object containing a temporary secret. Never log it.
Uses production message admission, Worker, Runtime, model adapter and session
commits. Only synthetic originals are seeded; the material Worker must produce
the representation through the real isolated Docker processor.
"""
import json
import os
from pathlib import Path
import sys
import time
import uuid


def history_exposes_citation(history, run_id, citation_id):
    """Match the actual current browser projection input, not arbitrary text."""
    return any(
        citation.get("citationId") == citation_id
        and citation.get("sourceUrl") == f"/api/citations/{citation_id}"
        for run in history.get("agentRuns", []) if run.get("id") == run_id
        for citation in run.get("citations", [])
    )


def verify_persistence(run_id, *, revoke=False):
    """No paid calls; rerun after restarting the isolated services."""
    if os.environ.get("MCP_E2E_ISOLATED") != "1":
        raise RuntimeError("Requires isolated acceptance deployment")
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "packages/api"))
    os.environ.setdefault("DJANGO_SETTINGS_MODULE", "api.settings")
    import django
    django.setup()
    from django.test import Client
    from app_core.models import AgentRun, SessionCitationProjection, MaterialEvidenceReceipt, ProviderCredential, DerivedRepresentation
    from app_core.session_event import rebuild_agent_run_citation_projection
    from app_core.material_contract import sha256_bytes
    from app_core.tests import streaming_response_bytes

    run = AgentRun.objects.get(pk=run_id)
    before = set(SessionCitationProjection.objects.filter(agent_run=run).values_list("citationId", flat=True))
    after = rebuild_agent_run_citation_projection(run)
    browser = Client(HTTP_HOST="localhost")
    browser.force_login(run.user)
    previews = []
    for citation in after:
        response = browser.get(f"/api/citations/{citation.citationId}/preview")
        representation = DerivedRepresentation.objects.get(pk=citation.representationId)
        expected_hash = representation.previewPdfSha256 or representation.canonicalTextSha256
        previews.append(response.status_code == 200 and sha256_bytes(streaming_response_bytes(response)) == expected_hash)
    history = browser.get(f"/api/sessions/{run.session_id}/history").json()
    checks = {"completed": run.status == "completed", "stableCitationIds": bool(before) and before == {c.citationId for c in after},
        "receiptPersisted": MaterialEvidenceReceipt.objects.filter(call__agent_run=run).exists(),
        "previewMatches": bool(previews) and all(previews),
        "temporaryCredentialRemoved": not ProviderCredential.objects.filter(provider_id=run.modelConfig.provider_id).exists(),
        "historyExposesCitation": any(history_exposes_citation(history, run.id, c.citationId) for c in after)}
    print(json.dumps({"stage": "persistence", "runId": run.id, "checks": checks}), flush=True)
    if revoke:
        from app_core.models import WorkspaceMembership
        WorkspaceMembership.objects.filter(workspace=run.workspace, user=run.user).delete()
        snapshot = browser.get(f"/api/sessions/{run.session_id}/agent-runs/{run.id}/citations")
        denied = snapshot.status_code == 404 and all(
            browser.get(f"/api/citations/{citation.citationId}/preview").status_code == 404
            for citation in after)
        print(json.dumps({"stage": "revocation", "snapshotAndPreviewsDenied": denied}), flush=True)
        checks["revocationDenied"] = denied
    return all(checks.values())


def main():
    if os.environ.get("MCP_E2E_ISOLATED") != "1":
        raise RuntimeError("Requires isolated acceptance deployment")
    configuration = json.loads(sys.stdin.readline())
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "packages/api"))
    os.environ.setdefault("DJANGO_SETTINGS_MODULE", "api.settings")
    import django
    django.setup()
    from django.contrib.auth.models import User
    from django.core.files.base import ContentFile
    from django.core.files.storage import default_storage
    from django.test import Client
    from app_core.models import (Workspace, WorkspaceMembership, ModelConfig, ModelProvider,
        ProviderCredential, AgentRun, UserLibraryObject, SessionAssetLink,
        ProcessingSpecification, DerivedRepresentation, MaterialEvidenceReceipt,
        SessionCitationProjection, SessionEvent, ModelRunLog)
    from app_core.credentials import encrypt_credential_secret
    from app_core.testing import create_session
    from app_core.assets import captured_input_fields
    from app_core.material_contract import sha256_bytes
    from app_core.material_identity import processing_spec_digest, representation_id

    user = User.objects.create_user(username="mcp-live-" + uuid.uuid4().hex)
    workspace = Workspace.objects.create(name="Isolated material acceptance", createdBy=user)
    WorkspaceMembership.objects.create(workspace=workspace, user=user, role="owner")
    session = create_session(workspace=workspace, owner=user)
    provider = ModelProvider.objects.create(displayName="Isolated DeepSeek", api="openai-completions",
                                           apiBase="https://api.deepseek.com")
    credential = ProviderCredential.objects.create(provider=provider, displayName="Temporary",
        encryptedSecret=encrypt_credential_secret(configuration.pop("secret")), createdBy=user, updatedBy=user)
    run = None
    browser = Client(HTTP_HOST="localhost")
    browser.force_login(user)
    try:
        model = ModelConfig.objects.create(provider=provider, modelName="deepseek-v4-flash",
            displayName="Live material probe", resolvedApi=provider.api, resolvedApiBase=provider.apiBase,
            contextTokens=131072, maxOutputTokens=2048)
        nonce = uuid.uuid4().hex
        content = f"The acceptance code is {nonce}.\n".encode()
        key = default_storage.save("mcp-e2e/material.txt", ContentFile(content))
        item = UserLibraryObject.objects.create(owner=user, displayName="acceptance.txt", objectKind="file",
            contentType="text/plain", sizeBytes=len(content), sha256=sha256_bytes(content), storageKey=key,
            contentGeneration=1, status="ready")
        link = SessionAssetLink.objects.create(workspace=workspace, session=session, userLibraryObject=item,
            attachedBy=user, capturedDisplayName=item.displayName, capturedContentType=item.contentType,
            **captured_input_fields(item))
        # Seed only the original. MCP admission must trigger real platform processing.
        from app_core.models import MaterialProcessingTask
        assert not DerivedRepresentation.objects.filter(ownerId=item.pk).exists()
        response = browser.post(f"/api/workspaces/{workspace.id}/sessions/{session.id}/messages",
            data=json.dumps({"operationId": uuid.uuid4().hex,
                "text": "Use list_materials and read_material to read the attached acceptance file. "
                "Report its acceptance code and include the exact citationId returned by read_material in your final answer. "
                "If processing is pending, use get_operation and then read_material again when completed. "
                "Do not use filesystem or legacy knowledge tools. Finish after reading it.",
                "attachmentRefs": [link.id], "modelConfigRef": model.id}), content_type="application/json")
        if response.status_code not in {200, 201, 202}:
            raise RuntimeError(f"Message admission failed: {response.status_code} {response.content[:300]!r}")
        run = AgentRun.objects.get(pk=response.json()["agentRunId"])
        print(json.dumps({"stage": "admitted", "runId": run.id}), flush=True)
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            run.refresh_from_db()
            if run.status in {"completed", "failed", "cancelled", "interrupted"}:
                break
            if ModelRunLog.objects.filter(agentRunId=run.id).count() >= 6:
                raise RuntimeError("Model request budget reached")
            time.sleep(1)
        events = list(SessionEvent.objects.filter(agent_run=run).order_by("agent_run_sequence").values_list("payload", flat=True))
        receipts = list(MaterialEvidenceReceipt.objects.filter(call__agent_run=run))
        citations = list(SessionCitationProjection.objects.filter(agent_run=run))
        history = browser.get(f"/api/sessions/{session.id}/history").json()
        answers = [e["payload"]["modelMarkdown"] for e in events
                   if e.get("type") == "assistant_message" and e.get("payload", {}).get("status") == "done"]
        answer = "\n".join(answers)
        checks = {"completed": run.status == "completed", "receiptPersisted": bool(receipts),
            "realProcessingCompleted": MaterialProcessingTask.objects.filter(payload__inputIdentity__ownerId=item.pk, executionBackend="platform", status="completed", attemptCount__gte=1).exists(),
            "citationProjected": bool(citations), "answerContainsUnknownCode": nonce in answer,
            "answerReferencesCitation": any(c.citationId in answer for c in citations),
            "historyExposesCitation": any(history_exposes_citation(history, run.id, c.citationId) for c in citations),
            "hasToolSuccess": any(e.get("type") == "tool_result" and e.get("payload", {}).get("resultState") == "successWithOutput" for e in events)}
        previews = []
        for citation in citations:
            preview = browser.get(f"/api/citations/{citation.citationId}/preview")
            if preview.status_code == 200:
                from app_core.tests import streaming_response_bytes
                representation = DerivedRepresentation.objects.get(pk=citation.representationId)
                expected_hash = representation.previewPdfSha256 or representation.canonicalTextSha256
                previews.append(sha256_bytes(streaming_response_bytes(preview)) == expected_hash)
            else:
                previews.append(False)
        checks["previewMatches"] = bool(previews) and all(previews)
        print(json.dumps({"stage": "result", "status": run.status, "checks": checks,
            "events": [{"type": e.get("type"), "tool": e.get("payload", {}).get("toolName")} for e in events],
            "usage": list(ModelRunLog.objects.filter(agentRunId=run.id).values("status", "promptTokens", "completionTokens", "error"))}), flush=True)
        if not all(checks.values()):
            raise RuntimeError("Live material acceptance failed")
    finally:
        # Stop further paid calls even if admission, execution or assertions fail.
        ModelProvider.objects.filter(pk=provider.pk).update(enabled=False)
        credential.delete()
        if run is not None:
            run.refresh_from_db()
            if run.status not in {"completed", "failed", "cancelled", "interrupted"}:
                browser.post(f"/api/sessions/{session.id}/agent-runs/{run.id}/cancel")


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--verify-revocation":
        raise SystemExit(0 if verify_persistence(sys.argv[2], revoke=True) else 1)
    if len(sys.argv) == 3 and sys.argv[1] == "--verify":
        raise SystemExit(0 if verify_persistence(sys.argv[2]) else 1)
    if len(sys.argv) != 1:
        raise SystemExit("Use stdin for a live run, or --verify RUN_ID without a credential")
    main()
