import copy
import unittest
from unittest.mock import patch
import boundary

class BoundaryRestartTests(unittest.TestCase):
 def setUp(self):
  self.cfg=boundary.parse_marker(boundary.marker_for(8,'a'*32,60));self.run={'agentRunId':'run','sessionId':'session'}
  self.old={'environmentInstanceId':'instance','controllerId':'run','controllerEpoch':3,'controllerPurpose':'execute'}
  self.current={**self.old,'controllerEpoch':4};self.calls=[];self.results=[]
  for i in range(5):
   self.calls.append({'callId':boundary.call_id(self.cfg,i),'turnId':'turn-'+str(i),'sequence':10+i*3,'toolName':'bash','normalizedInput':{'command':boundary.command_for(self.cfg,i)}})
   if i<4:self.results.append({'callId':boundary.call_id(self.cfg,i),'turnId':'turn-'+str(i),'sequence':11+i*3,'resultState':'successWithOutput','modelContent':boundary.token(self.cfg,i)})
  self.before={**self.run,'environmentId':'environment','instanceId':'instance','containerId':'container','runStatus':'running','fence':self.old,'mainRequestCount':5,'calls':self.calls,'results':self.results}
  self.after={**self.before,'fence':self.current};self.previous={**self.run,'environmentId':'environment','instanceId':'instance','containerId':'container','backgroundTaskIds':['background']}
  self.task={'taskId':'original-task','operationId':'3:opaque-original-operation','scope':{'kind':'run','runId':'run'},'state':'running','cleanupComplete':False}
 def query(self,task=None,output=None):
  return {'status':'success','result':{'kind':'task','task':task or self.task,'gap':False,'earliestCursor':0,'output':[{'stream':'stdout','data':list((output if output is not None else boundary.release_token(self.cfg)+'\n').encode())}]}}
 def execute(self,after=None,query_after=None,empty_first=False,stale_ok=True):
  order=[];states=iter([self.before,self.before if empty_first else (after or self.after),after or self.after]);queries=0
  def observe():return next(states)
  def rpc(container,fence,operation,**kw):
   nonlocal queries
   order.append(('rpc',operation,fence['controllerEpoch']))
   if operation=='list':return {'status':'success','result':{'kind':'listed','tasks':[self.task]}}
   if fence['controllerEpoch']==3:
    queries+=1
    if queries> (2 if empty_first else 1):return {'status':'error','error':'task_controller_fenced'} if stale_ok else self.query()
    if empty_first and queries==1:return self.query(output='')
    return self.query()
   return query_after or self.query()
  states_runtime=iter([{'Running':True,'OOMKilled':False,'Pid':1,'StartedAt':'old'},{'Running':True,'OOMKilled':False,'Pid':2,'StartedAt':'new'}])
  def release(container,config):order.append(('release',container,config['nonce']))
  def probe(*args):order.append(('probe',));return {'backgroundStillRunning':True}
  with patch.object(boundary.time,'sleep'):
   proof=boundary.execute_restart(self.run,self.cfg,self.previous,observe,lambda:next(states_runtime),lambda:order.append(('restart',)),rpc,release,lambda n,v:order.append(('save',n)),probe)
  return order,proof
 def test_original_running_task_new_epoch_and_old_fence_rejection_precede_release(self):
  order,proof=self.execute();self.assertEqual(proof['task']['taskId'],'original-task');self.assertTrue(proof['released'])
  self.assertLess(order.index(('restart',)),order.index(('rpc','query',4)))
  self.assertLess(order.index(('rpc','query',4)),order.index(('probe',)))
  self.assertLess(order.index(('probe',)),order.index(('release','container','a'*32)))
 def test_output_not_yet_flushed_is_retryable_before_restart_only(self):
  order,proof=self.execute(empty_first=True);self.assertTrue(proof['released']);self.assertEqual(order.count(('restart',)),1)
 def test_replacement_task_never_releases(self):
  with self.assertRaisesRegex(RuntimeError,'missing/replaced'):
   self.execute(query_after=self.query(task={**self.task,'taskId':'another'}))
 def test_canonical_result_committed_before_release_is_failure(self):
  after=copy.deepcopy(self.after);after['results'].append({'callId':boundary.call_id(self.cfg,4)})
  with self.assertRaisesRegex(RuntimeError,'before release'):self.execute(after=after)
 def test_new_model_request_before_original_result_is_failure(self):
  with self.assertRaisesRegex(RuntimeError,'recalled model'):self.execute(after={**self.after,'mainRequestCount':6})
 def test_changed_instance_never_releases(self):
  with self.assertRaisesRegex(RuntimeError,'changed original identity'):self.execute(after={**self.after,'instanceId':'other'})
 def test_old_fence_must_be_actually_rejected(self):
  with self.assertRaisesRegex(RuntimeError,'old boundary query fence'):self.execute(stale_ok=False)
 def test_duplicate_call_and_wrong_command_fail(self):
  for key,value in [('callId',self.calls[0]['callId']),('normalizedInput',{'command':'printf fake'})]:
   bad=copy.deepcopy(self.before);bad['calls'][4][key]=value
   with self.assertRaisesRegex(RuntimeError,'identity/command/order'):boundary.validate_facts(bad,self.cfg,self.run)
 def test_timeout_preserves_without_restart_or_release(self):
  actions=[]
  with patch.object(boundary.time,'monotonic',side_effect=[0,61]),self.assertRaisesRegex(RuntimeError,'not reached'):
   boundary.execute_restart(self.run,self.cfg,self.previous,lambda:None,lambda:None,lambda:actions.append('restart'),lambda *a,**k:None,lambda *a:actions.append('release'),lambda *a:None,lambda *a:None)
  self.assertEqual(actions,[])
if __name__=='__main__':unittest.main()
