"""Strict Runtime root manifests preserve owner scope without selecting checkpoints here."""
from copy import deepcopy
from types import SimpleNamespace

from django.test import SimpleTestCase

from .workspace_snapshot_roots import validate_roots,WorkspaceRootsUnavailable


class WorkspaceSnapshotRootsContractTests(SimpleTestCase):
    def test_selected_checkpoint_may_reuse_an_earlier_immutable_object(self):
        session=SimpleNamespace(pk="session-1",workspace_id="workspace-1")
        root={"agentRunId":"run-1","authorizationDigest":"sha256:"+"a"*64,"checkpointId":"checkpoint-new",
            "objectRef":"workspaces/workspace-1/sessions/session-1/agent-runs/run-1/execution-checkpoints/"+"b"*64+"/"+"c"*64+".snapshot",
            "snapshotSha256":"sha256:"+"c"*64,"snapshotSizeBytes":20,"expandedSizeBytes":1,"fileCount":1}
        manifest={"schema":"runtime.workspace_snapshot.roots.v1","sessionId":session.pk,"sessionSequence":1,
            "authorityFingerprint":"sha256:"+"d"*64,"protectedAgentRunIds":["run-1"],"roots":[root]}
        self.assertEqual(validate_roots(session,manifest),manifest)
        for change in ({"schema":"old"},{"rawAuthority":{}},{"sessionId":"other"},
                       {"protectedAgentRunIds":["run-1","run-1"]}):
            with self.subTest(change=change),self.assertRaises(WorkspaceRootsUnavailable):
                validate_roots(session,manifest|change)
        bad=deepcopy(manifest)
        bad["roots"][0]["objectRef"]=root["objectRef"].replace("workspace-1","other-workspace")
        with self.assertRaises(WorkspaceRootsUnavailable):
            validate_roots(session,bad)
