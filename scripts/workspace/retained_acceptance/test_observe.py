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
        fence={'environmentInstanceId':'instance','controllerOwner':{'kind':'agentRun','runId':'run-a'},
            'controllerEpoch':7,'controllerPurpose':'execute'}
        response={'schema':'environment.task.v3','status':'success',
            'result':{'kind':'listed','tasks':[]}}
        def service(command, **kwargs):
            self.assertEqual(command,['docker','exec','-i','--user','0:0','container-a',
                '/opt/centaeris/bin/execution_agent','environment-task-rpc'])
            self.assertEqual(json.loads(kwargs['input']),{'schema':'environment.task.v3',
                'request':{'operation':'query','fence':fence,'taskId':'task-a','cursor':0,'maxBytes':4096}})
            return SimpleNamespace(stdout=json.dumps(response))
        with patch.object(observe.subprocess,'run',side_effect=service):
            self.assertEqual(observe.task_rpc('container-a',fence,'query',
                taskId='task-a',cursor=0,maxBytes=4096),response)

    def test_rpc_rejects_older_response_and_preserves_current_fence_error(self):
        for schema in ('environment.task.v1','environment.task.v2'):
            response={'schema':schema,'status':'success','result':{'kind':'listed','tasks':[]}}
            with self.subTest(schema=schema),patch.object(observe.subprocess,'run',return_value=SimpleNamespace(stdout=json.dumps(response))):
                with self.assertRaisesRegex(RuntimeError,'response schema invalid'):
                    observe.task_rpc('container-a',{})
        response={'schema':'environment.task.v3','status':'error','error':'task_controller_fenced'}
        with patch.object(observe.subprocess,'run',return_value=SimpleNamespace(stdout=json.dumps(response))):
            self.assertEqual(observe.task_rpc('container-a',{}),response)

    def test_probe_uses_task_original_epoch_with_valid_operation_identity(self):
        previous={'fence':{'environmentInstanceId':'instance','controllerOwner':{'kind':'agentRun','runId':'run-a'},
            'controllerEpoch':2,'controllerPurpose':'cleanup'},'backgroundTaskIds':['task-a'],
            'taskService':{'result':{'tasks':[{'taskId':'task-a','operationId':'1:tool-start'}]}}}
        fence=observe.stale_execute_fence(previous)
        self.assertEqual(fence['controllerEpoch'],1)
        self.assertEqual(fence['controllerPurpose'],'execute')
        self.assertEqual(fence['controllerOwner'],{'kind':'agentRun','runId':'run-a'})
        for action in ('start','input','terminate'):
            operation=observe.stale_operation_id(fence,action)
            self.assertTrue(operation.startswith('1:'))
            self.assertLessEqual(len(operation),160)

    def test_probe_refuses_noncanonical_historical_task_identity(self):
        previous={'backgroundTaskIds':['task-a'],'taskService':{'result':{'tasks':[
            {'taskId':'task-a','operationId':'bad-prefix:start'}]}}}
        with self.assertRaisesRegex(RuntimeError,'canonical operation'):
            observe.stale_execute_fence(previous)

class StrictTerminalTests(unittest.TestCase):
    def state(self):
        return {'runStatus':'completed','coreTerminal':{'type':'agent_run_completed'},
            'releaseReceiptCount':1,'cleanupComplete':True,'coreTerminalCount':1}

    def test_exact_completed_terminal_and_release_required(self):
        observe.validate_terminal(self.state(),False)
        for changes in ({'runStatus':'failed','coreTerminal':{'type':'agent_run_failed','reasonType':'task_history_capacity'}},
                {'releaseReceiptCount':0},{'cleanupComplete':False},{'coreTerminalCount':2}):
            with self.subTest(changes=changes),self.assertRaises(RuntimeError):
                observe.validate_terminal(self.state()|changes,True)

    def test_history_requires70_actual_completed_commands(self):
        state=self.state()|{'historyAttemptedCommands':70,'historySuccessfulCommands':70,
            'workloadObservation':{'attemptedCommands':70,'completedCommands':70,'distinctCallIds':70}}
        self.assertEqual(observe.history_observation(state)['completedCommands'],70)
        for changes in ({'historySuccessfulCommands':21},{'historyAttemptedCommands':69},
                {'workloadObservation':None},{'runStatus':'failed'}):
            with self.subTest(changes=changes),self.assertRaises(RuntimeError):
                observe.history_observation(state|changes)

class StoredHistoryObservationTests(unittest.TestCase):
    def test_embedded_database_observer_derives_metrics_from_actual_record_shapes(self):
        import contextlib
        import io
        from types import ModuleType
        records=[]
        for index in range(21):
            call='call-'+str(index)
            records += [SimpleNamespace(eventId='call-event-'+str(index),payload={'type':'tool_call',
                'payload':{'callId':call,'toolName':'bash','normalizedInput':{'command':'printf acceptance-history-ok'}}}),
                SimpleNamespace(eventId='result-event-'+str(index),payload={'type':'tool_result',
                    'payload':{'callId':call,'toolName':'bash','resultState':'successWithOutput','modelContent':'acceptance-history-ok'}})]
        records.append(SimpleNamespace(eventId='empty-call',payload={'type':'tool_call',
            'payload':{'callId':'empty-call','toolName':'bash','normalizedInput':{'command':'true'}}}))
        records.append(SimpleNamespace(eventId='empty-success',payload={'type':'tool_result',
            'payload':{'callId':'empty-call','toolName':'bash','resultState':'successNoOutput','modelContent':''}}))
        records.append(SimpleNamespace(eventId='capacity-terminal',createdAtMs=100,
            payload={'type':'agent_run_failed','payload':{'reasonType':'task_history_capacity'}}))
        env=SimpleNamespace(id='env',controller_run_id='run',controller_purpose='cleanup',controller_released=True,controller_epoch=6)
        run=SimpleNamespace(id='run',session_id='session',workspace_id='workspace',environment=env,status='failed',prompt='DIAGNOSTIC_HISTORY')
        cursor=SimpleNamespace(execute=lambda *a:None,fetchall=unittest.mock.Mock(side_effect=[
            [('instance',)],[('container',268435456,1000,128,67108864,'host')]]),fetchone=lambda:('instance',True))
        connection=SimpleNamespace(cursor=lambda:contextlib.nullcontext(cursor))
        models=ModuleType('app_core.models')
        models.AgentRun=SimpleNamespace(objects=SimpleNamespace(select_related=lambda *a:SimpleNamespace(get=lambda **kw:run)))
        models.SessionEvent=SimpleNamespace(objects=SimpleNamespace(filter=lambda **kw:SimpleNamespace(order_by=lambda *a:records)))
        models.AgentRunEnvironmentRelease=SimpleNamespace(objects=SimpleNamespace(get=lambda **kw:SimpleNamespace(id='release'),
            filter=lambda **kw:SimpleNamespace(count=lambda:1)))
        conf=ModuleType('django.conf');conf.settings=SimpleNamespace(DATABASES={'default':{'NAME':'retained_acceptance'}})
        db=ModuleType('django.db');db.connection=connection
        class StopAfterDatabaseObservation(Exception):pass
        def command(argv,**kwargs):
            with patch.dict(sys.modules,{'django.conf':conf,'django.db':db,'app_core.models':models}),contextlib.redirect_stdout(io.StringIO()) as output:
                exec(compile(argv[-1],'<actual-database-observer>','exec'),{})
            observed=json.loads(output.getvalue())
            self.assertEqual(observed['historySuccessfulCommands'],21)
            self.assertEqual(observed['historyAttemptedCommands'],21)
            self.assertEqual(observed['successfulOrdinaryCommands'],22)
            self.assertEqual(observed['coreTerminal']['reasonType'],'task_history_capacity')
            self.assertEqual(observed['coreTerminal']['eventId'],'capacity-terminal')
            self.assertEqual(observed['releaseReceiptCount'],1)
            self.assertIs(observed['cleanupComplete'],True)
            raise StopAfterDatabaseObservation
        with patch.object(sys,'argv',['observe','--compose','fixture','--run','run','--result','unused',
                '--previous','previous']),patch.object(observe.subprocess,'run',side_effect=command):
            with self.assertRaises(StopAfterDatabaseObservation):observe.main()


if __name__=='__main__':
    unittest.main()
