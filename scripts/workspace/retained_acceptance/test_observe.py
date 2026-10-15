import importlib.util
from pathlib import Path
import unittest
import json
import sys
from types import SimpleNamespace
from unittest.mock import patch

spec=importlib.util.spec_from_file_location('observe',Path(__file__).with_name('observe.py'))
observe=importlib.util.module_from_spec(spec)
spec.loader.exec_module(observe)


class StaleProbeTests(unittest.TestCase):
    def test_observer_emits_executable_django_shell_code(self):
        class StopBeforeExecution(Exception):
            pass
        def validate_command(command, **kwargs):
            self.assertEqual(command[:3],['docker','compose','-f'])
            self.assertEqual(command[-3:-1],['shell','-c'])
            compile(command[-1],'<observer-django-shell>','exec')
            raise StopBeforeExecution
        with patch.object(sys,'argv',['observe','--compose','fixture-compose.json',
                '--run','fixture-run','--result','fixture-result.json']), \
                patch.object(observe.subprocess,'run',side_effect=validate_command) as execute:
            with self.assertRaises(StopBeforeExecution):
                observe.main()
            self.assertEqual(execute.call_count,1)

    def test_rpc_uses_current_protocol_and_exact_target_and_fence(self):
        fence={'environmentInstanceId':'instance','controllerId':'run-a',
            'controllerEpoch':7,'controllerPurpose':'execute'}
        response={'schema':'environment.task.v2','status':'success',
            'result':{'kind':'listed','tasks':[]}}
        def service(command, **kwargs):
            self.assertEqual(command,['docker','exec','-i','--user','0:0','container-a',
                '/opt/centaeris/bin/execution_agent','environment-task-rpc'])
            self.assertEqual(json.loads(kwargs['input']),{'schema':'environment.task.v2',
                'request':{'operation':'query','fence':fence,'taskId':'task-a','cursor':0,'maxBytes':1024}})
            return SimpleNamespace(stdout=json.dumps(response))
        with patch.object(observe.subprocess,'run',side_effect=service):
            self.assertEqual(observe.task_rpc('container-a',fence,'query',
                taskId='task-a',cursor=0,maxBytes=1024),response)

    def test_rpc_rejects_older_response_and_preserves_current_fence_error(self):
        response={'schema':'environment.task.v1','status':'success','result':{'kind':'listed','tasks':[]}}
        with patch.object(observe.subprocess,'run',return_value=SimpleNamespace(stdout=json.dumps(response))):
            with self.assertRaisesRegex(RuntimeError,'response schema invalid'):
                observe.task_rpc('container-a',{})
        response={'schema':'environment.task.v2','status':'error','error':'task_controller_fenced'}
        with patch.object(observe.subprocess,'run',return_value=SimpleNamespace(stdout=json.dumps(response))):
            self.assertEqual(observe.task_rpc('container-a',{}),response)

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
