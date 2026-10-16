import json
from pathlib import Path
import tempfile
import unittest
from cross_retry import wait_for_release, load_resume

class CrossWaitTests(unittest.TestCase):
    def sample(self, sequence=1, status='running', **changes):
        return dict(agentRunId='run',sessionId='session',runStatus=status,
            highestRunSequence=sequence,releaseReceiptCount=0,cleanupComplete=False,**changes)

    def exercise(self, samples, final=lambda: 'observed', total=900):
        now=[0]
        def status():
            value=next(samples);now[0]+=30;return value
        return wait_for_release({'agentRunId':'run','sessionId':'session'},status,final,
            clock=lambda:now[0],sleep=lambda _:None,total_seconds=total,idle_seconds=120)

    def test_progressing_history_can_finish_after_old_120_second_limit(self):
        samples=[self.sample(i) for i in range(1,12)]
        samples.append(self.sample(12,'completed')|{'releaseReceiptCount':1,'cleanupComplete':True})
        self.assertEqual(self.exercise(iter(samples)),'observed')

    def test_no_progress_is_bounded(self):
        with self.assertRaisesRegex(RuntimeError,'no progress'):
            self.exercise(iter([self.sample()]*10))

    def test_progress_does_not_remove_total_deadline(self):
        with self.assertRaisesRegex(RuntimeError,'total deadline'):
            self.exercise(iter([self.sample(i) for i in range(20)]),total=150)

    def test_failed_run_stops_without_waiting_or_observing(self):
        def final():self.fail('failed Run must not be observed as successful')
        with self.assertRaisesRegex(RuntimeError,'terminal failure'):
            self.exercise(iter([self.sample(status='failed')]),final)

    def test_queued_run_can_advance_but_failed_core_fact_stops_immediately(self):
        samples=[self.sample(status='queued'),self.sample(2,'completed')|{'releaseReceiptCount':1,'cleanupComplete':True}]
        self.assertEqual(self.exercise(iter(samples)),'observed')
        with self.assertRaisesRegex(RuntimeError,'terminal failure'):
            self.exercise(iter([self.sample()|{'coreTerminalType':'agent_run_failed'}]))

    def test_strict_observation_failure_is_not_retried(self):
        calls=[]
        def final():calls.append(1);raise RuntimeError('strict result mismatch')
        with self.assertRaisesRegex(RuntimeError,'strict result mismatch'):
            self.exercise(iter([self.sample(status='completed')|{'releaseReceiptCount':1,'cleanupComplete':True}]),final)
        self.assertEqual(calls,[1])

    def test_wrong_identity_and_progress_regression_are_rejected(self):
        for samples in ([self.sample()|{'agentRunId':'other'}],[self.sample(2),self.sample(1)]):
            with self.subTest(samples=samples),self.assertRaises(RuntimeError):self.exercise(iter(samples))

class ResumeTests(unittest.TestCase):
    def fixture(self, root):
        e=root/('evidence-'+'a'*32);e.mkdir()
        shared={'sessionId':'session','environmentId':'env','instanceId':'instance','containerId':'container'}
        for prefix,run in [('acceptance-a','a'),('acceptance-b','b'),('acceptance-use-0','history')]:
            (e/(prefix+'-nonce.json')).write_text(json.dumps({'agentRunId':run,'sessionId':'session','status':'accepted'}))
        for name,run in [('observed-a','a'),('observed-b','b')]:
            (e/(name+'.json')).write_text(json.dumps(shared|{'agentRunId':run,'runStatus':'completed'}))
        return e

    def test_reuses_original_admissions_and_rejects_another_container(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);e=self.fixture(root)
            v=load_resume(root,e,'container')
            self.assertEqual(v['history']['agentRunId'],'history')
            self.assertEqual(v['a']['agentRunId'],'a')
            self.assertEqual(v['b']['agentRunId'],'b')
            with self.assertRaises(RuntimeError):load_resume(root,e,'other')

    def test_duplicate_admission_or_retirement_cannot_resume(self):
        for name in ('acceptance-use-0-other.json','retirement-request.json'):
            with tempfile.TemporaryDirectory() as tmp:
                root=Path(tmp);e=self.fixture(root)
                (e/name).write_text('{}')
                with self.assertRaises(RuntimeError):load_resume(root,e,'container')


class ResumeDriverTests(unittest.TestCase):
    def exercise(self, foreign_inventory=False):
        import sys
        from types import SimpleNamespace
        from unittest.mock import patch
        import run as driver
        class BeforeRetirement(Exception):pass
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);e=ResumeTests().fixture(root);project='retainedacceptfixture';cid='c'*64
            (root/'manifest.json').write_text(json.dumps({'project':project,'binaryDirectory':tmp,'binarySha256':{},'source':tmp,'sourceRevision':'product-head','apiPort':1,'runtimePort':2}))
            (root/'compose.json').write_text(json.dumps({'name':project,'services':{'api':{'labels':{'centaeris.acceptance.project':project}}}}))
            (root/'synthetic.env').write_text('ENVIRONMENT_MAINTENANCE_TOKEN='+'m'*40+'\nINTERNAL_API_TOKEN='+'i'*40+'\n')
            for name in ('observed-a','observed-b'):
                p=e/(name+'.json');v=json.loads(p.read_text());v.update(containerId=cid,successfulOrdinaryCommands=1,hostId='host',fence={'controllerEpoch':4});p.write_text(json.dumps(v))
            (e/'commands.jsonl').write_text('original evidence\n')
            admissions=[];restarts=[];observations=[]
            def execute(argv,**kwargs):
                output=''
                if argv[0]=='git' and argv[-2:]==['rev-parse','HEAD']:output='product-head'
                if argv[:3]==['docker','ps','-aq']:output='f'*64 if foreign_inventory else (cid if '--no-trunc' in argv else cid[:12])
                if argv[:2]==['docker','inspect']:
                    output=json.dumps([{'Id':cid,'State':{'Running':True},'Config':{'Labels':{'centaeris.managed':'true','centaeris.environment_id':'env','centaeris.instance_id':'instance'}}}])
                if argv[-2:]==['restart','runtime']:restarts.append(argv)
                if len(argv)>1 and Path(argv[1]).name=='admit.py':
                    admissions.append(argv[argv.index('--text')+1]);Path(argv[argv.index('--result')+1]).write_text(json.dumps({'agentRunId':'new-'+str(len(admissions)),'sessionId':'session','status':'accepted'}))
                if len(argv)>1 and Path(argv[1]).name=='run_status.py':
                    output=json.dumps({'schema':'retained.acceptance.run.status.v1','environmentId':'env','instanceId':'instance','agentRunId':argv[argv.index('--run')+1],'sessionId':'session','runStatus':'completed','highestRunSequence':10,'releaseReceiptCount':1,'cleanupComplete':True})
                if len(argv)>1 and Path(argv[1]).name=='observe.py':
                    run=argv[argv.index('--run')+1];observations.append(run)
                    state={'agentRunId':run,'sessionId':'session','environmentId':'env','instanceId':'instance','containerId':'container','hostId':'host','fence':{'controllerEpoch':8},'runStatus':'completed','coreTerminal':{'type':'agent_run_completed'},'successfulOrdinaryCommands':70 if run=='history' else 1,'historyObservation':{'attemptedCommands':70,'completedCommands':70,'distinctCallIds':70}}
                    Path(argv[argv.index('--result')+1]).write_text(json.dumps(state))
                return SimpleNamespace(returncode=0,stdout=output,stderr='')
            args=['run','--fixture',tmp,'--resume-cross-evidence',str(e),'--expected-container',cid,'--acknowledge-tmpfs-loss']
            with patch.object(sys,'argv',args),patch.object(driver.subprocess,'run',side_effect=execute),patch.object(driver.urllib.request,'urlopen',side_effect=BeforeRetirement):
                if foreign_inventory:
                    with self.assertRaisesRegex(SystemExit,'exact'):driver.main()
                else:
                    with self.assertRaises(BeforeRetirement):driver.main()
            self.assertEqual(restarts,[])
            self.assertEqual((e/'commands.jsonl').read_text(),'original evidence\n')
            if foreign_inventory:self.assertEqual(admissions,[])
            else:
                self.assertEqual(admissions,['ACCEPTANCE_RUN_B']*97)
                self.assertEqual(len(observations),98)
                self.assertEqual(observations[0],'history')
                self.assertEqual(len(set(observations)),98)

    def test_resume_never_readmits_or_restarts_the_first_three_runs(self):self.exercise()
    def test_another_managed_container_refuses_retry_before_admission(self):self.exercise(True)

if __name__=='__main__':unittest.main()
