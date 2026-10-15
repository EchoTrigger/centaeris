import importlib.util
from pathlib import Path
import unittest
spec=importlib.util.spec_from_file_location('acceptance_run',Path(__file__).with_name('run.py'))
driver=importlib.util.module_from_spec(spec);spec.loader.exec_module(driver)

class HistoryDriverTests(unittest.TestCase):
    def test_history_requires70_successes_and_completed_terminal(self):
        state={'runStatus':'completed','coreTerminal':{'type':'agent_run_completed'},
            'historyObservation':{'attemptedCommands':70,'completedCommands':70,'distinctCallIds':70}}
        self.assertEqual(driver.history_metrics(state)['completedCommands'],70)
        for changes in ({'runStatus':'failed','coreTerminal':{'type':'agent_run_failed','reasonType':'task_history_capacity'}},
                {'historyObservation':{'attemptedCommands':70,'completedCommands':21,'distinctCallIds':70}},
                {'historyObservation':None}):
            with self.subTest(changes=changes),self.assertRaises(RuntimeError):
                driver.history_metrics(state|changes)


class LongDriverSequenceTests(unittest.TestCase):
    def exercise(self, progress_failure=None, cleanup_only=None, missing_summary=False):
        import hashlib,json,sys,tempfile
        from types import SimpleNamespace
        from unittest.mock import patch
        class StopBeforeAuthorizedRetirement(Exception):pass
        digest=hashlib.sha256(b'Keep all acceptance commands bounded.\n').hexdigest()
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder);project='retainedacceptfixture'
            (root/'manifest.json').write_text(json.dumps({'project':project,'binaryDirectory':folder,
                'binarySha256':{},'source':folder,'sourceRevision':'fixture-head','apiPort':1,'runtimePort':2}))
            (root/'compose.json').write_text(json.dumps({'name':project,'services':{
                'api':{'labels':{'centaeris.acceptance.project':project}}}}))
            (root/'synthetic.env').write_text('ENVIRONMENT_MAINTENANCE_TOKEN='+'m'*40+'\nINTERNAL_API_TOKEN='+'i'*40+'\n')
            admissions=[];commands=[];progress_calls=0;successful_samples=0;inspections=0
            def execute(argv,**kwargs):
                nonlocal progress_calls,successful_samples,inspections
                commands.append(argv);output=''
                if argv[0]=='git' and argv[-2:]==['rev-parse','HEAD']:output='fixture-head'
                if len(argv)>1 and Path(argv[1]).name=='admit.py':
                    admissions.append(argv[argv.index('--text')+1])
                    Path(argv[argv.index('--result')+1]).write_text(json.dumps({'agentRunId':'run-'+str(len(admissions)),'sessionId':'session'}))
                if len(argv)>1 and Path(argv[1]).name=='progress.py':
                    progress_calls+=1
                    if progress_failure and progress_calls==1:
                        return SimpleNamespace(returncode=progress_failure,stdout='',stderr=json.dumps({
                            'schema':'retained.acceptance.observation.v1','status':'notReady' if progress_failure==75 else 'fatal','reason':'fixture'}))
                    successful_samples+=1
                    completed=successful_samples>=5 or (cleanup_only=='completed' and successful_samples==2)
                    purpose='cleanup' if completed or (cleanup_only and successful_samples>=2) else 'execute'
                    Path(argv[argv.index('--result')+1]).write_text(json.dumps({'runStatus':'completed' if completed else 'running',
                        'completedCommands':100 if successful_samples==1 else (1000 if completed else (900 if successful_samples==2 else (950 if successful_samples==3 else 975))),'containerId':'instance-cid',
                        'highestSessionSequence':40 if successful_samples==1 else (100 if successful_samples==2 else 250+(successful_samples-3)*100),
                        'durableCompaction':None if successful_samples==1 or missing_summary else {
                            'compactionId':'compact-1','summaryMessageId':'summary-1','summarySha256':'sha256:'+'a'*64,
                            'summaryBytes':100,'sequence':80,'sessionEventId':'event-80','failureCount':0,
                            'requestBound':{'requestId':'request-compact','sequence':70,'manifestDigest':'sha256:'+'b'*64,'compactionPromptDigest':'sha256:'+'c'*64},
                            'lastMainObservation':{'requestId':'main-before' if successful_samples==2 else 'main-after-'+str(successful_samples),'sequence':85 if successful_samples==2 else 210+(successful_samples-3)*100,'manifestDigest':'sha256:'+'d'*64,'summaryContentDigest':'sha256:'+'e'*64,'successfulCommand':{'callId':'command-'+str(successful_samples),'commandIndex':899 if successful_samples==2 else (949 if successful_samples==3 else 974),'callSequence':90 if successful_samples==2 else 220+(successful_samples-3)*100,'resultSequence':95 if successful_samples==2 else 230+(successful_samples-3)*100}}},
                        'fence':{'environmentInstanceId':'instance','controllerId':'run-2','controllerEpoch':3 if successful_samples<=2 else (5 if purpose=='cleanup' else 4),'controllerPurpose':purpose},
                        'live':{'backgroundTaskIds':['background']},'store':{'indexedRunTasks':301 if successful_samples==1 else 3000}}))
                if len(argv)>1 and Path(argv[1]).name=='observe.py':
                    Path(argv[argv.index('--result')+1]).write_text(json.dumps({'successfulOrdinaryCommands':1000 if len(admissions)==2 else 0,
                        'environmentId':'environment','instanceId':'instance','hostId':'host','containerId':'instance-cid',
                        'agentsFileSha256':digest,'fence':{'controllerEpoch':5},
                        'workloadObservation':{'attemptedCommands':1000,'completedCommands':1000,'distinctCallIds':1000,
                            'agentsInstructions':{'mainRequestsWithAgentsInstructions':1001,'agentsFileSha256':digest}}}))
                if argv[-3:]==['ps','-q','runtime']:output='runtime-cid'
                if argv[:2]==['docker','inspect']:
                    inspections+=1;output=json.dumps({'Running':True,'Pid':10+min(inspections,2),'StartedAt':str(min(inspections,2))})
                if 'logs' in argv:
                    output='\n'.join(json.dumps({'kind':'automaticInstructionsObserved','commandIndex':i,'instructionSha256':digest}) for i in range(1000))
                if 'head' in argv:output='\n'.join('%04d'%i for i in range(1000))+'\n'
                return SimpleNamespace(returncode=0,stdout=output,stderr='')
            with patch.object(sys,'argv',['run','--fixture',folder,'--long-run','--acknowledge-tmpfs-loss']), \
                    patch.object(driver.subprocess,'run',side_effect=execute),patch.object(driver.time,'sleep'), \
                    patch.object(driver,'probe_reclaimed_controller',return_value={'allStaleMutationsRejected':True}) as stale, \
                    patch.object(driver.urllib.request,'urlopen',side_effect=StopBeforeAuthorizedRetirement):
                if progress_failure==1:
                    with self.assertRaisesRegex(RuntimeError,'fatal live observation'):driver.main()
                    self.assertEqual(progress_calls,1)
                    return
                if cleanup_only or missing_summary:
                    with self.assertRaisesRegex(RuntimeError,'long Run lacks complete work'):driver.main()
                    stale.assert_not_called()
                    return
                with self.assertRaises(StopBeforeAuthorizedRetirement):driver.main()
            self.assertEqual(admissions,['ACCEPTANCE_RUN_A','ACCEPTANCE_LONG_RUN'])
            self.assertEqual(sum(argv[-2:]==['restart','runtime'] for argv in commands),2)
            self.assertEqual(stale.call_args.args[1]['controllerEpoch'],3)
            self.assertEqual(stale.call_args.args[2]['controllerEpoch'],4)
            evidence=next(root.glob('evidence-*'))
            self.assertEqual(json.loads((evidence/'long-side-effect-proof.json').read_text())['lines'],1000)
            self.assertEqual(json.loads((evidence/'automatic-instructions.json').read_text())['distinctCommandIndices'],1000)
            proof=json.loads((evidence/'long-compaction-before-restart.json').read_text())
            self.assertEqual(proof['completedCommands'],900)
            self.assertEqual(proof['durableCompaction']['summaryMessageId'],'summary-1')
            restart=json.loads((evidence/'long-runtime-restart.json').read_text())
            self.assertEqual(restart['compactionRecovery']['originalCompactionRequestId'],'request-compact')
            self.assertEqual(restart['postRestartBaseline']['highestSessionSequence'],250)
            self.assertEqual(restart['compactionRecovery']['postRestartMainObservation']['sequence'],310)
            self.assertEqual(restart['compactionRecovery']['postRestartCommandResult']['resultSequence'],330)
            progresses=[i for i,argv in enumerate(commands) if len(argv)>1 and Path(argv[1]).name=='progress.py' and (not progress_failure or i>0)]
            restarts=[i for i,argv in enumerate(commands) if argv[-2:]==['restart','runtime']]
            # Actual driver must observe the summary-bearing second sample first.
            self.assertGreater(restarts[-1],progresses[1 if not progress_failure else 2])
            self.assertTrue(any('--compaction-proof' in argv for argv in commands if len(argv)>1 and Path(argv[1]).name=='progress.py'))

    def test_real_driver_requires_mid_run_restart_and_complete_evidence_before_retirement(self):
        self.exercise()

    def test_no_summary_before_completion_explicitly_fails_without_restart(self):
        self.exercise(missing_summary=True)

    def test_fatal_live_invariant_stops_before_a_later_success_can_hide_it(self):
        self.exercise(progress_failure=1)

    def test_explicit_not_ready_can_retry_then_complete(self):
        self.exercise(progress_failure=75)


    def test_cleanup_fence_never_substitutes_for_mid_run_execute_reclaim(self):
        for status in ('running','completed'):
            with self.subTest(status=status):self.exercise(cleanup_only=status)


class BoundedProgressEvidenceTests(unittest.TestCase):
    def test_cap_retained_samples_without_losing_full_observation_maxima(self):
        evidence=driver.ProgressEvidence()
        for index in range(1000):
            evidence.record({'completedCommands':index,'store':{'indexedRunTasks':10000 if index==0 else index},
                'live':{'hotTaskRecords':64,'activeTasks':1}})
        result=evidence.snapshot()
        self.assertEqual(result['observationCount'],1000)
        self.assertEqual(len(result['retainedSamples']),256)
        self.assertEqual(result['maxima']['store.indexedRunTasks'],10000)
        self.assertEqual(result['retainedSamples'][0]['completedCommands'],744)

class RetryClassificationTests(unittest.TestCase):
    def test_only_typed_not_ready_exit_is_retryable(self):
        import json
        from types import SimpleNamespace
        good={'schema':'retained.acceptance.observation.v1','status':'notReady','reason':'queued'}
        self.assertTrue(driver.retryable_observation(SimpleNamespace(returncode=75,stderr=json.dumps(good))))
        for code,value in ((1,good),(75,good|{'status':'fatal'}),(75,good|{'schema':'other'}),
                (75,[]),(75,'untyped error')):
            with self.subTest(code=code,value=value):
                self.assertFalse(driver.retryable_observation(SimpleNamespace(returncode=code,stderr=json.dumps(value))))

class CommandTranscriptTests(unittest.TestCase):
    def test_append_sequence_preserves_existing_entries_and_multiline_outputs(self):
        import json,tempfile
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory() as folder:
            path=Path(folder)/'commands.jsonl'
            for sequence in (1,2):
                driver.append_command_evidence(path,sequence,['tool',str(sequence)],
                    SimpleNamespace(returncode=0,stdout='line1\nline2\n',stderr=''))
            entries=[json.loads(line) for line in path.read_text().splitlines()]
            self.assertEqual([row['sequence'] for row in entries],[1,2])
            self.assertEqual(entries[0]['stdout'],'line1\nline2\n')
            self.assertEqual(entries[1]['command'],['tool','2'])

    def test_oversized_output_keeps_bounded_diagnostic_and_fails(self):
        import json,tempfile
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory() as folder:
            path=Path(folder)/'commands.jsonl'
            with self.assertRaisesRegex(RuntimeError,'evidence output exceeded'):
                driver.append_command_evidence(path,1,['tool'],
                    SimpleNamespace(returncode=0,stdout='x'*(1024*1024+1),stderr=''))
            row=json.loads(path.read_text())
            self.assertTrue(row['outputTruncated'])
            self.assertEqual(row['stdoutBytes'],1024*1024+1)
            self.assertLessEqual(len(row['stdout'].encode()),1024*1024)

class CompactedRestartBoundaryTests(unittest.TestCase):
    def sample(self):
        return {'completedCommands':700,'runStatus':'running','highestSessionSequence':90,'fence':{'controllerPurpose':'execute'},
            'durableCompaction':{'compactionId':'compact-1','summaryMessageId':'summary-1','summarySha256':'sha256:'+'a'*64,'summaryBytes':100,'sequence':80,'sessionEventId':'event-80','failureCount':0,
                'requestBound':{'requestId':'request-compact','sequence':70,'manifestDigest':'sha256:'+'b'*64,'compactionPromptDigest':'sha256:'+'c'*64},
                'lastMainObservation':{'requestId':'main-before','sequence':85,'manifestDigest':'sha256:'+'d'*64,'summaryContentDigest':'sha256:'+'e'*64}}}
    def test_100_without_summary_cannot_restart(self):
        self.assertFalse(driver.should_restart_long_run({'completedCommands':100,'runStatus':'running','durableCompaction':None},None))
    def test_committed_and_observed_summary_allows_restart_without_changing_work_target(self):
        self.assertTrue(driver.should_restart_long_run(self.sample(),None))
        self.assertFalse(driver.should_restart_long_run(self.sample(),{}))
    def test_summary_must_be_bound_to_real_model_request_and_used(self):
        for changes in ({'requestBound':None},{'lastMainObservation':None}):
            value=self.sample();value['durableCompaction']|=changes
            self.assertFalse(driver.should_restart_long_run(value,None))
