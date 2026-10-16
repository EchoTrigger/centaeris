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


class DurableCompactionTests(unittest.TestCase):
    def fixture(self):
        import hashlib,json
        from types import SimpleNamespace
        def stored(value,domain):
            raw=json.dumps(value,separators=(',',':'))
            key='sha256:'+hashlib.sha256(('centaeris.model_observation_'+domain+'.v1\0'+raw).encode()).hexdigest()
            return key,raw
        summary='committed synthetic summary'
        content=stored({'kind':'message','message':{'messageId':'summary-1','role':'system','content':summary}},'content')
        prompt_digest='sha256:'+'a'*64
        manifests=[stored({'parentDigest':None,'observationCount':1,'changes':[{'index':0,'kind':'compaction_prompt','contentDigest':prompt_digest}]},'manifest'),
            stored({'parentDigest':None,'observationCount':1,'changes':[{'index':0,'kind':'message','contentDigest':content[0]}]},'manifest')]
        def event(n,kind,body):return SimpleNamespace(agent_run_sequence=n,eventId='event-'+str(n),payload={'type':kind,'payload':body})
        events=[event(10,'model_request_started',{'purpose':'compaction','requestId':'compact-request','observations':{'manifestDigest':manifests[0][0]}}),
            event(20,'compaction',{'compactionId':'compact-1','summaryMessageId':'summary-1','summaryMarkdown':summary,'firstKeptMessageId':'tail'}),
            event(30,'model_request_started',{'purpose':'main','requestId':'main-after','observations':{'manifestDigest':manifests[1][0]}}),
            event(40,'tool_call',{'toolName':'bash','callId':'command-700','normalizedInput':{'command':'printf acceptance-long-0700'}}),
            event(50,'tool_result',{'callId':'command-700','resultState':'successWithOutput','modelContent':'acceptance-long-0700'})]
        class Cursor:
            snapshot=[(summary,{'kind':'context_compaction','compaction_id':'compact-1','first_kept_message_id':'tail'},300,'System')]
            contents=[content]
            failure_count=0
            def execute(self,sql,params):self.sql=sql;self.params=params
            def fetchall(self):
                if 'session_runtime_snapshots' in self.sql:return self.snapshot
                if 'model_observation_manifests' in self.sql:return manifests
                if 'content_json FROM' in self.sql:return self.contents
                return [('compaction_prompt',12345)]
            def fetchone(self):return (self.failure_count,)
        return events,Cursor(),manifests

    def test_persisted_summary_and_real_request_are_bound_without_full_prompt_or_snapshot(self):
        import json
        events,cursor,_=self.fixture()
        facts=progress.durable_compaction_facts(events,'session',cursor)
        self.assertEqual(facts['summaryMessageId'],'summary-1')
        self.assertEqual(facts['snapshotMessagePosition'],300)
        self.assertEqual(facts['requestBound']['requestId'],'compact-request')
        self.assertEqual(facts['lastMainObservation']['requestId'],'main-after')
        self.assertNotIn('committed synthetic summary',json.dumps(facts))

    def test_reduced_snapshot_empty_metadata_preserves_canonical_cas_summary_identity(self):
        events,cursor,_=self.fixture()
        summary=cursor.snapshot[0][0]
        cursor.snapshot=[(summary,{},300,'System')]
        facts=progress.durable_compaction_facts(events,'session',cursor,'summary-1')
        self.assertEqual(facts['summaryMessageId'],'summary-1')
        self.assertEqual(facts['requestBound']['requestId'],'compact-request')
        self.assertEqual(facts['lastMainObservation']['requestId'],'main-after')
        # Earlier real main observation proves the reduced representation. It
        # never substitutes for a newer request beyond the recovery watermark.
        waiting=progress.durable_compaction_facts(events,'session',cursor,'summary-1',50)
        self.assertEqual(waiting['summarySha256'],facts['summarySha256'])
        self.assertIsNone(waiting['lastMainObservation'])

    def test_reconstructed_summary_requires_an_existing_canonical_main_reference(self):
        events,cursor,_=self.fixture()
        cursor.snapshot=[(cursor.snapshot[0][0],{},300,'System')]
        # Even a valid CAS content row alone is not evidence it reached main.
        self.assertIsNone(progress.durable_compaction_facts(events[:2],'session',cursor))
        with self.assertRaisesRegex(RuntimeError,'no canonical main observation'):
            progress.durable_compaction_facts(events[:2],'session',cursor,'summary-1')

    def test_reconstructed_summary_still_requires_exact_cas_hash_and_commit(self):
        events,cursor,_=self.fixture()
        cursor.snapshot=[(cursor.snapshot[0][0],{},300,'System')]
        self.assertIsNone(progress.durable_compaction_facts([events[0]]+events[2:],'session',cursor))
        cursor.contents=[(cursor.contents[0][0],cursor.contents[0][1]+' ')]
        with self.assertRaisesRegex(RuntimeError,'observation digest mismatch'):
            progress.durable_compaction_facts(events,'session',cursor,'summary-1')

    def test_present_metadata_conflicts_are_never_treated_as_reconstruction(self):
        for metadata in ({'kind':'wrong','compaction_id':'compact-1','first_kept_message_id':'tail'},
                {'kind':'context_compaction','compaction_id':'different','first_kept_message_id':'tail'},
                {'kind':'context_compaction','compaction_id':'compact-1','first_kept_message_id':'different'},
                {'kind':'context_compaction'}):
            events,cursor,_=self.fixture();cursor.snapshot=[(cursor.snapshot[0][0],metadata,300,'System')]
            with self.subTest(metadata=metadata),self.assertRaisesRegex(RuntimeError,'identity changed'):
                progress.durable_compaction_facts(events,'session',cursor,'summary-1')

    def test_reconstruction_cannot_hide_wrong_snapshot_role_or_duplicate_identity(self):
        for role in ('system','User'):
            events,cursor,_=self.fixture();cursor.snapshot=[(cursor.snapshot[0][0],{},300,role)]
            with self.subTest(role=role),self.assertRaisesRegex(RuntimeError,'identity changed'):
                progress.durable_compaction_facts(events,'session',cursor,'summary-1')
        events,cursor,_=self.fixture();row=(cursor.snapshot[0][0],{},300,'System');cursor.snapshot=[row,row]
        with self.assertRaisesRegex(RuntimeError,'persisted summary missing or duplicated'):
            progress.durable_compaction_facts(events,'session',cursor,'summary-1')
        events,cursor,_=self.fixture();events.insert(2,events[1])
        with self.assertRaisesRegex(RuntimeError,'required summary missing or duplicated'):
            progress.durable_compaction_facts(events,'session',cursor,'summary-1')

    def test_no_commit_is_an_explicit_unready_observation(self):
        events,cursor,_=self.fixture()
        self.assertIsNone(progress.durable_compaction_facts([events[0]],'session',cursor))
        cursor.snapshot=[]
        self.assertIsNone(progress.durable_compaction_facts(events,'session',cursor))
        with self.assertRaisesRegex(RuntimeError,'persisted summary'):
            progress.durable_compaction_facts(events,'session',cursor,'summary-1')

    def test_changed_summary_or_wrong_model_purpose_cannot_qualify(self):
        events,cursor,_=self.fixture();cursor.snapshot=[('changed',{},300,'System')]
        with self.assertRaisesRegex(RuntimeError,'identity changed'):
            progress.durable_compaction_facts(events,'session',cursor)
        events,cursor,_=self.fixture();events[0].payload['payload']['purpose']='main'
        with self.assertRaisesRegex(RuntimeError,'request binding missing'):
            progress.durable_compaction_facts(events,'session',cursor)

    def test_corrupted_cas_and_missing_post_restart_main_are_not_recovery(self):
        events,cursor,manifests=self.fixture();manifests[0]=(manifests[0][0],manifests[0][1]+' ')
        with self.assertRaisesRegex(RuntimeError,'manifest digest mismatch'):
            progress.durable_compaction_facts(events,'session',cursor)
        events,cursor,_=self.fixture()
        facts=progress.durable_compaction_facts(events,'session',cursor,'summary-1',30)
        self.assertIsNone(facts['lastMainObservation'])

    def test_recovery_requires_same_summary_request_new_epoch_and_new_main(self):
        events,cursor,_=self.fixture();facts=progress.durable_compaction_facts(events,'session',cursor)
        before={'durableCompaction':facts,'highestSessionSequence':25,'completedCommands':700,
            'fence':{'controllerId':'same-run','environmentInstanceId':'instance','controllerEpoch':3}}
        current={'durableCompaction':facts,'completedCommands':701,'fence':before['fence']|{'controllerEpoch':4}}
        baseline={'highestSessionSequence':26,'completedCommands':700,'fence':current['fence']}
        proof=progress.compaction_recovery_proof(before,current,baseline)
        self.assertEqual(proof['originalCompactionRequestId'],'compact-request')
        self.assertEqual(proof['postRestartMainObservation']['sequence'],30)
        for changed in ({'summarySha256':'sha256:'+'f'*64},{'requestBound':facts['requestBound']|{'requestId':'regenerated'}}):
            with self.subTest(changed=changed),self.assertRaises(RuntimeError):
                progress.compaction_recovery_proof(before,current|{'durableCompaction':facts|changed},baseline)
        self.assertIsNone(progress.compaction_recovery_proof(before|{'highestSessionSequence':30},current,baseline|{'highestSessionSequence':30}))
        with self.assertRaisesRegex(RuntimeError,'newer same-Run controller'):
            progress.compaction_recovery_proof(before,current|{'fence':before['fence']},baseline)

    def test_old_runtime_tail_after_pre_restart_sample_cannot_prove_new_main(self):
        events,cursor,_=self.fixture();facts=progress.durable_compaction_facts(events,'session',cursor)
        before={'durableCompaction':facts,'highestSessionSequence':25,'completedCommands':700,
            'fence':{'controllerId':'same-run','environmentInstanceId':'instance','controllerEpoch':3}}
        new_fence=before['fence']|{'controllerEpoch':4}
        # Main sequence30 and old task results can commit between pre-sample25 and restart.
        # They are already visible in the first post-restart read at sequence55.
        baseline={'highestSessionSequence':55,'completedCommands':701,'fence':new_fence}
        current={'durableCompaction':facts,'completedCommands':702,'fence':new_fence}
        self.assertIsNone(progress.compaction_recovery_proof(before,current,baseline))
