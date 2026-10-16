import contextlib,importlib.util,io,json,subprocess,sys,unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch
spec=importlib.util.spec_from_file_location('run_status',Path(__file__).with_name('run_status.py'))
status=importlib.util.module_from_spec(spec);spec.loader.exec_module(status)
def fact(**changes):
 return dict(schema='retained.acceptance.run.status.v1',agentRunId='run-a',sessionId='session-a',environmentId='environment-a',runStatus='running',highestRunSequence=10,releaseReceiptCount=0,cleanupComplete=False,instanceId='instance-a',coreTerminalType=None,**changes)
class RunStatusTests(unittest.TestCase):
 def test_exact_read_only_child_request_and_last_json_are_used(self):
  expected=fact();calls=[]
  def execute(argv,**kwargs):
   calls.append(argv)
   self.assertEqual(argv[:9],['docker','compose','-f','owned-compose.json','exec','-T','api','/app/.venv/bin/python','manage.py'])
   self.assertEqual(argv[9:11],['shell','-c'])
   compile(argv[-1],'<django status observer>','exec')
   self.assertEqual(kwargs,dict(stdin=subprocess.DEVNULL,capture_output=True,text=True,check=True,timeout=30))
   return SimpleNamespace(stdout='Django shell preamble\n'+json.dumps(expected)+'\n')
  with patch.object(status.subprocess,'run',side_effect=execute):
   self.assertEqual(status.read_status(Path('owned-compose.json'),'run-a'),expected)
  self.assertEqual(len(calls),1)
 def test_child_error_and_timeout_propagate_without_status_fabrication(self):
  for error in (subprocess.CalledProcessError(1,['docker']),subprocess.TimeoutExpired(['docker'],30)):
   with self.subTest(error=error),patch.object(status.subprocess,'run',side_effect=error),self.assertRaises(type(error)):
    status.read_status(Path('fixture.json'),'run-a')
 def test_schema_identity_bad_scalars_unknown_fields_and_truncated_output_rejected(self):
  base=fact()
  values=[{**base,'schema':'retained.acceptance.run.status.v0'},{**base,'agentRunId':'other'},
   {**base,'sessionId':''},{**base,'highestRunSequence':True},{**base,'highestRunSequence':-1},
   {**base,'releaseReceiptCount':2},{**base,'cleanupComplete':1},{**base,'runStatus':'unknown'},
   {**base,'instanceId':12},{**base,'coreTerminalType':'tool_result'},{**base,'unknown':True}]
  for value in values:
   with self.subTest(value=value),patch.object(status.subprocess,'run',return_value=SimpleNamespace(stdout=json.dumps(value))),self.assertRaises(RuntimeError):
    status.read_status(Path('fixture.json'),'run-a')
  for output in ('',json.dumps(base)+'\ntrailing invalid', 'x'*65537):
   with self.subTest(output=output[:20]),patch.object(status.subprocess,'run',return_value=SimpleNamespace(stdout=output)),self.assertRaises(RuntimeError):
    status.read_status(Path('fixture.json'),'run-a')
 def test_unallocated_queued_status_is_explicit_and_failed_core_fact_is_preserved(self):
  for value in ({**fact(),'runStatus':'queued','instanceId':None,'environmentId':None,'highestRunSequence':0},
   {**fact(),'runStatus':'failed','coreTerminalType':'agent_run_failed'}):
   with patch.object(status.subprocess,'run',return_value=SimpleNamespace(stdout=json.dumps(value))):
    self.assertEqual(status.read_status(Path('fixture.json'),'run-a'),value)
 def test_main_stdout_is_exactly_one_status_json(self):
  output=io.StringIO();expected=fact()
  with patch.object(sys,'argv',['run_status','--compose','fixture.json','--run','run-a']),patch.object(status,'read_status',return_value=expected) as read,contextlib.redirect_stdout(output):status.main()
  read.assert_called_once_with(Path('fixture.json'),'run-a')
  self.assertEqual(output.getvalue(),json.dumps(expected)+'\n')
class DjangoStatusCodeTests(unittest.TestCase):
 def observe(self,allocations=None,receipts=None,terminal=None,environment_session='session-a'):
  run=SimpleNamespace(id='run-a',session_id='session-a',environment_id='environment-a',status='running',environment=SimpleNamespace(session_id=environment_session))
  allocations=allocations if allocations is not None else [('instance-a','environment-a','session-a')]
  receipts=receipts if receipts is not None else []
  terminals=terminal or [];commands=[]
  class Cursor:
   def __enter__(self):return self
   def __exit__(self,*args):return False
   def execute(self,sql,parameters=None):
    commands.append((sql,parameters))
    if sql=='SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY':self.rows=[]
    else:
     if not commands or commands[0][0]!='SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY':raise AssertionError('missing read-only transaction')
     if parameters!=['run-a']:raise AssertionError('query escaped exact Run')
     if sql.startswith('SELECT DISTINCT instance_id,environment_id,session_id FROM runtime.execution_instances '):self.rows=allocations
     elif sql.startswith('SELECT environment_id,instance_id,cleanup_complete FROM runtime.agent_run_cleanup_receipts '):self.rows=receipts
     else:raise AssertionError('unexpected database operation')
   def fetchall(self):return self.rows
  class Events:
   def filter(self,**kwargs):
    if kwargs.get('agent_run') is not run or kwargs.get('session_level') is not False:raise AssertionError('event scope changed')
    return self
   def aggregate(self,**kwargs):
    if kwargs!={'highest':('max','agent_run_sequence')}:raise AssertionError('expected scalar maximum, never history bodies')
    return {'highest':100000}
   def values_list(self,name,flat):
    if name!='payload__type' or flat is not True:raise AssertionError('only terminal type scalar permitted')
    return terminals
  def get(**kwargs):
   if kwargs!={'id':'run-a'}:raise AssertionError('exact Run required')
   return run
  agent=SimpleNamespace(objects=SimpleNamespace(select_related=lambda relation:SimpleNamespace(get=get)))
  releases=SimpleNamespace(objects=SimpleNamespace(filter=lambda **kwargs:SimpleNamespace(count=lambda:0)))
  modules={'django':SimpleNamespace(),'django.conf':SimpleNamespace(settings=SimpleNamespace(DATABASES={'default':{'NAME':'retained_acceptance'}})),
   'django.db':SimpleNamespace(connection=SimpleNamespace(cursor=Cursor),transaction=SimpleNamespace(atomic=contextlib.nullcontext)),
   'django.db.models':SimpleNamespace(Max=lambda field:('max',field)),'app_core':SimpleNamespace(),
   'app_core.models':SimpleNamespace(AgentRun=agent,AgentRunEnvironmentRelease=releases,SessionEvent=SimpleNamespace(objects=Events()))}
  def child(argv,**kwargs):
   output=io.StringIO()
   with patch.dict(sys.modules,modules),contextlib.redirect_stdout(output):exec(compile(argv[-1],'<actual observer child>','exec'),{})
   return SimpleNamespace(stdout=output.getvalue())
  with patch.object(status.subprocess,'run',side_effect=child):result=status.read_status(Path('fixture.json'),'run-a')
  return result,commands
 def test_actual_child_executes_read_only_scalar_queries_and_exact_bindings(self):
  result,commands=self.observe()
  self.assertEqual(result['highestRunSequence'],100000)
  self.assertEqual(result['instanceId'],'instance-a')
  self.assertEqual(result['cleanupComplete'],False)
  self.assertEqual(len(commands),3)
 def test_actual_child_rejects_session_allocation_receipt_and_duplicate_terminal_conflicts(self):
  cases=[{'environment_session':'foreign'}, {'allocations':[('instance-a','environment-a','foreign')]},
   {'allocations':[('instance-a','environment-a','session-a'),('instance-b','environment-a','session-a')]},
   {'receipts':[('environment-a','other-instance',True)]}, {'terminal':['agent_run_failed','agent_run_completed']}]
  for case in cases:
   with self.subTest(case=case),self.assertRaises(RuntimeError):self.observe(**case)
 def test_actual_child_reports_no_allocation_and_original_cleanup_fact_without_body_load(self):
  result,_=self.observe(allocations=[],receipts=[('environment-a',None,True)],terminal=['agent_run_failed'])
  self.assertIsNone(result['instanceId']);self.assertTrue(result['cleanupComplete'])
  self.assertEqual(result['coreTerminalType'],'agent_run_failed')

if __name__=='__main__':unittest.main()
