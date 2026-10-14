import importlib.util
from pathlib import Path
import unittest

spec=importlib.util.spec_from_file_location('observe',Path(__file__).with_name('observe.py'))
observe=importlib.util.module_from_spec(spec)
spec.loader.exec_module(observe)


class StaleProbeTests(unittest.TestCase):
    def test_probe_uses_task_original_epoch_with_valid_operation_identity(self):
        previous={'fence':{'environmentInstanceId':'instance','controllerId':'run-a',
            'controllerEpoch':2,'controllerPurpose':'cleanup'},'backgroundTaskIds':['task-a'],
            'taskService':{'result':{'tasks':[{'taskId':'task-a','operationId':'1:tool-start'}]}}}
        fence=observe.stale_execute_fence(previous)
        self.assertEqual(fence['controllerEpoch'],1)
        self.assertEqual(fence['controllerPurpose'],'execute')
        self.assertEqual(fence['controllerId'],'run-a')
        for action in ('start','input','terminate'):
            operation=observe.stale_operation_id(fence,action)
            self.assertTrue(operation.startswith('1:'))
            self.assertLessEqual(len(operation),160)

    def test_probe_refuses_noncanonical_historical_task_identity(self):
        previous={'backgroundTaskIds':['task-a'],'taskService':{'result':{'tasks':[
            {'taskId':'task-a','operationId':'bad-prefix:start'}]}}}
        with self.assertRaisesRegex(RuntimeError,'canonical operation'):
            observe.stale_execute_fence(previous)


if __name__=='__main__':
    unittest.main()
