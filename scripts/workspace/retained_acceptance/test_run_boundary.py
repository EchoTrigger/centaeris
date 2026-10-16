import hashlib,json,sys,tempfile,unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch
import run as driver
import boundary

class BoundaryDriverWiringTests(unittest.TestCase):
 def test_explicit_small_gate_never_enters_long_or_cross_history(self):
  class StopBeforeRetirement(Exception):pass
  nonce='a'*32;cfg=boundary.parse_marker(boundary.marker_for(8,nonce,180));calls=[];results=[]
  for i in range(8):
   calls.append({'callId':boundary.call_id(cfg,i),'turnId':'turn-'+str(i),'sequence':10+3*i,'toolName':'bash','normalizedInput':{'command':boundary.command_for(cfg,i)}})
   results.append({'callId':boundary.call_id(cfg,i),'turnId':'turn-'+str(i),'sequence':11+3*i,'resultState':'successWithOutput','modelContent':boundary.token(cfg,i)})
  with tempfile.TemporaryDirectory() as folder:
   root=Path(folder);project='retainedacceptfixture'
   (root/'manifest.json').write_text(json.dumps({'project':project,'binaryDirectory':folder,'binarySha256':{},'source':folder,'sourceRevision':'fixture-head','apiPort':1,'runtimePort':2}))
   (root/'compose.json').write_text(json.dumps({'name':project,'services':{'api':{'labels':{'centaeris.acceptance.project':project}}}}))
   (root/'synthetic.env').write_text('ENVIRONMENT_MAINTENANCE_TOKEN='+'m'*40+'\nINTERNAL_API_TOKEN='+'i'*40+'\n')
   admissions=[];commands=[]
   def execute(argv,**kwargs):
    commands.append(argv);output=''
    if argv[0]=='git' and argv[-2:]==['rev-parse','HEAD']:output='fixture-head'
    if len(argv)>1 and Path(argv[1]).name=='admit.py':
     admissions.append(argv[argv.index('--text')+1]);Path(argv[argv.index('--result')+1]).write_text(json.dumps({'agentRunId':'run-'+str(len(admissions)),'sessionId':'session'}))
    if len(argv)>1 and Path(argv[1]).name=='observe.py':
     Path(argv[argv.index('--result')+1]).write_text(json.dumps({'successfulOrdinaryCommands':8 if len(admissions)==2 else 0,'agentRunId':'run-'+str(len(admissions)),'sessionId':'session','environmentId':'environment','instanceId':'instance','hostId':'host','containerId':'container','backgroundTaskIds':['background'],'fence':{'controllerEpoch':5}}))
    if 'head' in argv:output='\n'.join('%04d'%i for i in range(8))+'\n'
    return SimpleNamespace(returncode=0,stdout=output,stderr='')
   facts={'agentRunId':'run-2','sessionId':'session','runStatus':'completed','calls':calls,'results':results}
   with patch.object(sys,'argv',['run','--fixture',folder,'--boundary-steps','8','--acknowledge-tmpfs-loss']),patch.object(driver.subprocess,'run',side_effect=execute),patch.object(driver.uuid,'uuid4',return_value=SimpleNamespace(hex=nonce)),patch.object(driver.boundary,'execute_restart',return_value={'callId':boundary.call_id(cfg,4)}) as restart,patch.object(driver.boundary,'read_state',return_value=facts),patch.object(driver.urllib.request,'urlopen',side_effect=StopBeforeRetirement):
    with self.assertRaises(StopBeforeRetirement):driver.main()
   self.assertEqual(admissions,['ACCEPTANCE_RUN_A',boundary.marker_for(8,nonce,180)])
   self.assertEqual(restart.call_count,1);self.assertEqual(restart.call_args.args[1],cfg)
   self.assertFalse(any(len(c)>1 and Path(c[1]).name=='progress.py' for c in commands))
   proof=json.loads(next(root.glob('evidence-*')).joinpath('boundary-completion-proof.json').read_text())
   self.assertEqual(proof['completedSteps'],8);self.assertEqual(proof['sideEffects']['lines'],8)
   self.assertEqual(proof['originalResult']['callId'],boundary.call_id(cfg,4))
if __name__=='__main__':unittest.main()
