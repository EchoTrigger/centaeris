import importlib.util
from pathlib import Path
import unittest
spec=importlib.util.spec_from_file_location('progress',Path(__file__).with_name('progress.py'))
progress=importlib.util.module_from_spec(spec);spec.loader.exec_module(progress)

class LiveObservationTests(unittest.TestCase):
    def tasks(self,count=64,active=1):
        tasks=[{'taskId':'background','state':'running','scope':{'kind':'environment'},'cleanupComplete':False}]
        tasks += [{'taskId':str(i),'state':'running' if i<active-1 else 'exited',
            'scope':{'kind':'run','runId':'run'},'cleanupComplete':i>=active-1} for i in range(count-1)]
        return {'status':'success','result':{'kind':'listed','tasks':tasks}}

    def test_live_bounds_allow64hot_and32active(self):
        value=progress.validate_live({'runStatus':'running'},self.tasks(active=32),['background'])
        self.assertEqual(value['hotTaskRecords'],64)
        self.assertEqual(value['activeTasks'],32)

    def test_missing_task_failed_run_and_resource_growth_reject(self):
        for status,tasks,ids in [('failed',self.tasks(),['background']),
                ('running',self.tasks(count=65),['background']),
                ('running',self.tasks(active=33),['background']),
                ('running',self.tasks(),['different'])]:
            with self.subTest(status=status,ids=ids),self.assertRaises(RuntimeError):
                progress.validate_live({'runStatus':status},tasks,ids)

class PrivateInspectionTests(unittest.TestCase):
    def metrics(self):
        return {'schema':'environment.task.store.inspection.v1','instanceId':'instance',
            'controllerId':'run','controllerEpoch':7,'hotTasks':64,'activeTasks':1,
            'indexedTasks':3001,'indexedRunTasks':3000,'controlDirectoryBytes':123456,
            'outputDirectoryBytes':1000}

    def invoke(self,metrics):
        import json
        from types import SimpleNamespace
        from unittest.mock import patch
        def execute(argv,**kw):
            self.assertEqual(argv,['docker','exec','--user','0:0','exact-container',
                '/opt/centaeris/bin/execution_agent','environment-task-store-inspect'])
            return SimpleNamespace(stdout=json.dumps(metrics))
        with patch.object(progress.subprocess,'run',side_effect=execute):
            return progress.inspect_store('exact-container',{'environmentInstanceId':'instance',
                'controllerId':'run','controllerEpoch':7})

    def test_exact_directory_budget_boundary_remains_available(self):
        self.assertEqual(self.invoke(self.metrics()|{'controlDirectoryBytes':8*1024*1024,
            'outputDirectoryBytes':72*1024*1024})['controlDirectoryBytes'],8*1024*1024)

    def test_fixed_exact_readonly_target_and_real_index_count(self):
        self.assertEqual(self.invoke(self.metrics())['indexedRunTasks'],3000)

    def test_unknown_schema_identity_invalid_count_and_hot_overflow_reject(self):
        for changes in ({'schema':'old'},{'instanceId':'wrong'},{'controllerEpoch':6},
                {'hotTasks':65},{'activeTasks':33},{'indexedRunTasks':True},
                {'indexedRunTasks':4000},{'unknown':1},{'controlDirectoryBytes':8*1024*1024+1},
                {'outputDirectoryBytes':72*1024*1024+1}):
            with self.subTest(changes=changes),self.assertRaises(RuntimeError):
                self.invoke(self.metrics()|changes)

class ReclaimedControllerTests(unittest.TestCase):
    def test_same_run_new_fence_rejects_every_old_mutation_and_preserves_task(self):
        from unittest.mock import patch
        old={'environmentInstanceId':'instance','controllerId':'same-run','controllerEpoch':1,'controllerPurpose':'execute'}
        current=old|{'controllerEpoch':2}
        calls=[]
        def rpc(container,fence,operation='list',**kwargs):
            calls.append((container,fence,operation,kwargs))
            if fence==old:return {'status':'error','error':'task_controller_fenced'}
            self.assertGreaterEqual(kwargs['maxBytes'],4096)
            return {'status':'success','result':{'kind':'task','task':{'taskId':'background','state':'running','cleanupComplete':False}}}
        with patch.object(progress,'task_rpc',side_effect=rpc):
            result=progress.probe_reclaimed_controller('exact-container',old,current,'background')
        self.assertTrue(result['backgroundStillRunning'])
        self.assertEqual([call[2] for call in calls],['list','start','input','terminate','endRun','query'])
        self.assertTrue(all(call[3]['operationId'].startswith('1:') for call in calls[1:5]))

    def test_no_mutation_if_same_epoch_or_initial_fence_probe_is_not_rejected(self):
        from unittest.mock import patch
        old={'environmentInstanceId':'instance','controllerId':'same-run','controllerEpoch':1}
        with patch.object(progress,'task_rpc',return_value={'status':'success'}) as rpc:
            with self.assertRaises(RuntimeError):progress.probe_reclaimed_controller('container',old,old,'background')
            rpc.assert_not_called()
            with self.assertRaises(RuntimeError):progress.probe_reclaimed_controller('container',old,old|{'controllerEpoch':2},'background')
            self.assertEqual(rpc.call_count,1)

class ObservationExitTests(unittest.TestCase):
    def test_only_explicit_not_ready_has_retry_exit(self):
        import contextlib,io,json
        from unittest.mock import patch
        for error,code,status in ((progress.ObservationNotReady('queued'),75,'notReady'),
                (RuntimeError('hot record limit exceeded'),1,'fatal')):
            with self.subTest(error=type(error).__name__),patch.object(progress,'main',side_effect=error),contextlib.redirect_stderr(io.StringIO()) as out:
                self.assertEqual(progress.cli(),code)
                self.assertEqual(json.loads(out.getvalue())['status'],status)

    def test_live_directory_capacity_overflow_is_fatal(self):
        for control,output in ((8*1024*1024+1,0),(0,72*1024*1024+1)):
            with self.subTest(control=control,output=output),self.assertRaises(RuntimeError):
                progress.validate_resource_bytes({'controlDirectoryBytes':control,'outputDirectoryBytes':output,
                    'instanceMemoryBytes':1,'instancePids':1})
