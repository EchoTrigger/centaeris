import contextlib,importlib.util,io,json,os,sys,types,unittest
from decimal import Decimal
from pathlib import Path
from unittest.mock import patch
SOURCE=Path(__file__).with_name('runsc_rotation_acceptance.py')
spec=importlib.util.spec_from_file_location('native_rotation',SOURCE);rotation=importlib.util.module_from_spec(spec);spec.loader.exec_module(rotation)
class DecimalCollectorTests(unittest.TestCase):
 def observe_total(self,total):
  class Cursor:
   def __enter__(self):return self
   def __exit__(self,*args):return False
   def execute(self,sql,args=None):self.sql=sql
   def fetchall(self):return [('1'*64,'instance-A','host-A',9007199254740993,'allocation-A',512)]
   def fetchone(self):
    if 'SELECT state,controller_epoch' in self.sql:return ('paused',1)
    if 'sum(memory_bytes)' in self.sql:return (1,total)
    return (0,)
  class Query:
   def select_related(self,*args):return self
   def get(self,**kwargs):return types.SimpleNamespace(environment_id='env-A',environment=types.SimpleNamespace(controller_epoch=1))
   def filter(self,**kwargs):return self
   def order_by(self,*args):return self
   def values_list(self,*args,**kwargs):return []
  modules={'django.conf':types.SimpleNamespace(settings=types.SimpleNamespace(DATABASES={'default':{'NAME':'retained_acceptance'}})),'django.db':types.SimpleNamespace(connection=types.SimpleNamespace(cursor=Cursor),transaction=types.SimpleNamespace(atomic=contextlib.nullcontext)),'app_core.models':types.SimpleNamespace(AgentRun=types.SimpleNamespace(objects=Query()),SessionEvent=types.SimpleNamespace(objects=Query()))}
  driver=rotation.FixtureDriver.__new__(rotation.FixtureDriver)
  def dc(*args):
   output=io.StringIO()
   with patch.dict(sys.modules,modules),contextlib.redirect_stdout(output):exec(args[-1],{})
   return output.getvalue()
  driver.dc=dc
  driver.inspect=lambda identifier:({'Paused':True,'Running':True,'Pid':321},{'centaeris.managed':'true','centaeris.environment_id':'env-A','centaeris.instance_id':'instance-A'},'runsc')
  driver.pid_metrics=lambda container,pid,limit:{'current':67,'max':limit,'eventsMax':0}
  row=driver.observe('A',{'agentRunId':'run-A','sessionId':'session-A'})
  return row
 def test_postgres_bigint_sum_stays_charged_and_serializes_as_integer(self):
  row=self.observe_total(Decimal('9007199254740993'))
  self.assertIs(type(row['residentMemoryBytes']),int)
  self.assertEqual(row['residentMemoryBytes'],9007199254740993)
  self.assertEqual(row['residentCount'],1)
  self.assertEqual(row['runningReservations'],0)
  self.assertTrue(row['paused'])
  self.assertEqual(row['residentPids'],512)
  self.assertEqual(row['pids'],{'current':67,'max':512,'eventsMax':0})
 def test_fractional_memory_sum_is_rejected_instead_of_truncated(self):
  with self.assertRaisesRegex(RuntimeError,'not an integer'):
   self.observe_total(Decimal('256.5'))
if __name__=='__main__':unittest.main()
