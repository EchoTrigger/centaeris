"""Setup owns an independent environment before any model Run is submitted."""
import unittest

import test_workspace_environment as fixture
from workspace_environment import OwnedEnvironment, WorkspaceEnvironment
from test_workspace_environment import profile_digest
from workspace_client import WorkspaceError


@unittest.skipUnless(WorkspaceEnvironment, 'requires Harbor')
class UnifiedEnvironmentTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        fixture.EnvironmentTests.setUp(self)
        def provision(session, environment, operation):
            self.provisioner.calls.append(('provision', operation))
            return OwnedEnvironment(session, environment, None, 'instance', 'container', 'host', 1,
                self.profile['imageDigest'], profile_digest(self.profile), 'resident', 1)
        self.provisioner.provision = provision

    async def test_setup_provisions_and_prepares_without_submitting_a_model_run(self):
        await self.environment.start()
        self.assertFalse(any(call[0] == 'submit' for call in self.client.calls))
        self.assertEqual(self.provisioner.calls[1][0], 'provision')
        self.assertTrue(any(call[0] == 'exec' for call in self.provisioner.calls))
        self.assertIsNone(self.environment._agent_run_id)
        self.assertIsNone(self.environment._owned.agent_run_id)
        self.assertIsNotNone(self.environment.workspace_binding)
        self.environment.bind_workspace_run('run')
        self.assertEqual(self.environment._owned.agent_run_id, 'run')
        with self.assertRaises(WorkspaceError):
            self.environment.bind_workspace_run('foreign')

    async def test_failed_setup_never_starts_model_work_and_keeps_cleanup_owned(self):
        self.provisioner.exec = lambda *a, **k: {'returnCode': 1, 'stdout': '', 'stderr': 'setup failed'}
        with self.assertRaises(WorkspaceError):
            await self.environment.start()
        self.assertFalse(any(call[0] == 'submit' for call in self.client.calls))
        self.assertIsNotNone(self.environment._owned)
        self.assertFalse(self.allocation.released)
