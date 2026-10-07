"""Synthetic wire-contract tests; no database, services, or developer keys."""

import copy
import json
import os
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from django.test import SimpleTestCase, override_settings

from .runtime_contract import (
    authorization_digest,
    authorization_signature,
    build_agent_run_authorization_payload,
    verify_agent_run_authorization_signature,
)


FIXTURES = Path(__file__).resolve().parents[3] / "tests/workspace/fixtures/agent_run_authorization/v1"
KEY = "test-key"


def fixture():
    return json.loads((FIXTURES / "valid.json").read_text(encoding="utf-8"))


def corpus():
    cases = json.loads((FIXTURES / "cases.json").read_text(encoding="utf-8"))
    assert isinstance(cases, list) and cases, "empty case corpus"
    ids = set()
    required = {
        "id", "changes", "signerError", "verifierStage", "verifierError",
        "rationale", "supportedSignerOutput",
    }
    for case in cases:
        assert required <= case.keys() <= required | {"digest", "signature"}
        assert case["id"] and case["id"] not in ids
        ids.add(case["id"])
        assert case["rationale"] and type(case["supportedSignerOutput"]) is bool
        assert case["verifierStage"] in {"accept", "deserialize", "validate"}
        assert bool(case["verifierError"]) == (case["verifierStage"] != "accept")
        if case["supportedSignerOutput"]:
            assert not case["signerError"] and case["verifierStage"] == "accept"
        assert ("digest" in case and "signature" in case) == (not case["signerError"])
    return cases


def payload_for(case):
    payload = fixture()
    for change in case["changes"]:
        assert set(change) in ({"path"}, {"path", "value"})
        assert change["path"].startswith("/")
        parts = change["path"][1:].split("/")
        target = payload
        for part in parts[:-1]:
            target = target[int(part)] if isinstance(target, list) else target[part]
        key = int(parts[-1]) if isinstance(target, list) else parts[-1]
        if "value" in change:
            target[key] = copy.deepcopy(change["value"])
        else:
            del target[key]
    return payload


class AgentRunAuthorizationContractTests(SimpleTestCase):
    def test_stable_signed_fixture(self):
        payload = fixture()
        self.assertEqual(
            authorization_digest(payload),
            "sha256:bd93e8ba466c0d9851e805dd2a8c8a5962b351065c4d982ae961ea0cdb0a6a9f",
        )
        self.assertEqual(
            authorization_signature(payload, KEY),
            "hmac-sha256:86d800a4e2517f1b169894646f9a7bb3297770235a7a3a0cbceffe1897969dd5",
        )
        self.assertEqual(
            authorization_digest(dict(reversed(list(payload.items())))),
            authorization_digest(payload),
        )

    def test_shared_corpus(self):
        for case in corpus():
            with self.subTest(case=case["id"]):
                payload = payload_for(case)
                if case["signerError"]:
                    with self.assertRaisesRegex(ValueError, case["signerError"]):
                        authorization_signature(payload, KEY)
                else:
                    self.assertEqual(authorization_digest(payload), case["digest"])
                    self.assertEqual(authorization_signature(payload, KEY), case["signature"])

    def test_authentication_rejects_tamper_and_wrong_key(self):
        payload = fixture()
        signature = authorization_signature(payload, KEY)
        verify_agent_run_authorization_signature(payload, KEY, signature)
        with self.assertRaisesRegex(ValueError, "signature mismatch"):
            verify_agent_run_authorization_signature(payload, "wrong-key", signature)
        payload["sessionId"] = "different_session"
        with self.assertRaisesRegex(ValueError, "signature mismatch"):
            verify_agent_run_authorization_signature(payload, KEY, signature)

    def test_builder_rejects_resources_outside_rust_transport_range(self):
        """Observe the real builder; isolate only asset/plugin I/O, not validation."""
        payload = fixture()
        session = SimpleNamespace(
            agent_id="centaeris", workspaceGeneration=7,
            workspaceSnapshotSha256=payload["sessionWorkspace"]["snapshotSha256"],
            workspaceSnapshotSizeBytes=13, workspaceExpandedSizeBytes=7,
            workspaceFileCount=1,
        )
        run = SimpleNamespace(
            workspace_id="ws_1", user_id="user_1", session=session,
            session_id="sess_1", id="agent_run_1", modelConfig_id="model_1",
            thinkingMode="high", workspace=object(), definition_version_id=None,
        )
        for setting, maximum in [
            ("SANDBOX_CPU_MILLI", 2**32 - 1),
            ("SANDBOX_PIDS_LIMIT", 2**32 - 1),
            ("SANDBOX_MEMORY_BYTES", 2**64 - 1),
            ("SANDBOX_DATA_TMPFS_BYTES", 2**64 - 1),
        ]:
            with (
                self.subTest(setting=setting),
                patch("app_core.assets.deferred_input_refs", return_value=payload["assetRefs"]),
                patch(
                    "app_core.runtime_contract.plugin_activation_for_workspace",
                    return_value=payload["pluginActivation"],
                ),
            ):
                with override_settings(**{setting: maximum}):
                    accepted = build_agent_run_authorization_payload(
                        run, "authorization_1", image_digest=payload["imageDigest"],
                    )
                    authorization_signature(accepted, KEY)
                with override_settings(**{setting: maximum + 1}):
                    with self.assertRaisesRegex(ValueError, "transport range"):
                        build_agent_run_authorization_payload(
                            run, "authorization_1", image_digest=payload["imageDigest"],
                        )

    def test_emit_python_signed_payloads_for_rust(self):
        destination = os.environ.get("AGENT_RUN_AUTHORIZATION_VECTORS")
        if destination is None:
            return  # Ordinary suite: no filesystem export; corpus assertions still run.
        vectors = []
        for case in corpus():
            if not case["signerError"]:
                payload = payload_for(case)
                vectors.append({
                    "id": case["id"],
                    "payloadJson": json.dumps(payload, ensure_ascii=False),
                    "digest": authorization_digest(payload),
                    "signature": authorization_signature(payload, KEY),
                })
        self.assertTrue(vectors)
        Path(destination).write_text(json.dumps(vectors, ensure_ascii=False), encoding="utf-8")
