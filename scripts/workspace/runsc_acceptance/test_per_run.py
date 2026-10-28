import contextlib
import copy
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import types
import unittest
from unittest.mock import Mock,patch

spec=importlib.util.spec_from_file_location('per_run_subject',Path(__file__).with_name('per_run.py'))
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)


class PhysicalCleanupTests(unittest.TestCase):
 def driver(self,*,allocations=None,receipts=None,holding=0):
  driver=object.__new__(module.PerRunFixtureDriver)
  driver.docker=['docker','--host','unix:///owned/docker.sock'];driver.base='http://127.0.0.1:18083'
  driver.command=Mock(return_value='')
  run=types.SimpleNamespace(id='run-a',session_id='session-a',environment_id='environment-a')
  allocations=[('instance-a','environment-a','session-a')] if allocations is None else allocations
  receipts=[('environment-a','instance-a',True,True,'a'*64,1900)] if receipts is None else receipts
  calls=[]
  class Cursor:
   def __enter__(self):return self
   def __exit__(self,*args):return False
   def execute(self,sql,parameters=None):self.index=len(calls);calls.append(parameters)
   def fetchall(self):return allocations if self.index==1 else receipts
   def fetchone(self):return (holding,)
  objects=Mock();objects.get.return_value=run
  modules={'django.conf':types.SimpleNamespace(settings=types.SimpleNamespace(DATABASES={'default':{'NAME':'retained_acceptance'}})),
   'django.db':types.SimpleNamespace(connection=types.SimpleNamespace(cursor=Cursor),transaction=types.SimpleNamespace(atomic=contextlib.nullcontext)),
   'app_core.models':types.SimpleNamespace(AgentRun=types.SimpleNamespace(objects=objects))}
  def sql(code):
   output=io.StringIO()
   with patch.dict(sys.modules,modules),contextlib.redirect_stdout(output):exec(code,{})
   return json.loads(output.getvalue())
  driver.sql=sql;driver.public_run_history=Mock(return_value=True)
  return driver,calls,objects
 def test_original_receipt_identity_is_bound_before_inventory_and_history_are_read(self):
  driver,calls,objects=self.driver()
  proof=driver.per_run_cleanup({'agentRunId':'run-a','sessionId':'session-a'})
  objects.get.assert_called_once_with(pk='run-a',session_id='session-a')
  self.assertEqual(calls[1:], [['run-a'],['run-a'],['instance-a','a'*64]])
  self.assertTrue(proof['physicalRemoved']);self.assertTrue(proof['containerAbsent']);self.assertTrue(proof['historyPreserved'])
  self.assertEqual(proof['containerId'],'a'*64);self.assertEqual(proof['residentHoldingCount'],0)
  driver.public_run_history.assert_called_once_with({'agentRunId':'run-a','sessionId':'session-a'},'environment-a')
 def test_missing_ambiguous_foreign_or_nonphysical_receipts_cannot_reach_absence_observation(self):
  good=('environment-a','instance-a',True,True,'a'*64,1900)
  cases=[{'allocations':[]},{'allocations':[('instance-a','foreign','session-a')]}, {'receipts':[]},{'receipts':[good,good]},
   {'receipts':[('environment-a','foreign',True,True,'a'*64,1900)]},
   {'receipts':[('environment-a','instance-a',True,False,'a'*64,1900)]}, {'holding':1}]
  for options in cases:
   with self.subTest(options=options):
    driver,_,_=self.driver(**options)
    with self.assertRaises(RuntimeError):driver.per_run_cleanup({'agentRunId':'run-a','sessionId':'session-a'})
    driver.command.assert_not_called();driver.public_run_history.assert_not_called()
 def test_inventory_errors_malformed_results_and_original_container_presence_reject(self):
  for inventory in ('not-a-container', 'a'*64, subprocess.CalledProcessError(1,['docker','ps'])):
   with self.subTest(inventory=inventory):
    driver,_,_=self.driver()
    if isinstance(inventory,Exception):driver.command.side_effect=inventory
    else:driver.command.return_value=inventory
    with self.assertRaises((RuntimeError,subprocess.CalledProcessError)):driver.per_run_cleanup({'agentRunId':'run-a','sessionId':'session-a'})
    driver.public_run_history.assert_not_called()
 def test_other_current_container_does_not_hide_original_absence(self):
  driver,_,_=self.driver();driver.command.return_value='b'*64+'\n'
  self.assertTrue(driver.per_run_cleanup({'agentRunId':'run-a','sessionId':'session-a'})['containerAbsent'])


class PublicHistoryTests(unittest.TestCase):
 def stream(self):
  events=[('tool_call',{'toolName':'bash','callId':'call-a'}),('tool_result',{'toolName':'bash','callId':'call-a','resultState':'successWithOutput'}),('artifact_published',{}),('agent_run_completed',{})]
  return [{'schema':'session.stream.item.v1','kind':'committed','agentRunId':'run-a','sourceSequence':index,
   'event':{'sessionId':'session-a','agentRunId':'run-a','sequence':index,'eventId':f'event-{index}','type':kind,'payload':payload}}
   for index,(kind,payload) in enumerate(events,1)]
 def driver(self,items):
  driver=object.__new__(module.PerRunFixtureDriver);driver.base='http://127.0.0.1:18083'
  driver.http=Mock(return_value={'sessionId':'session-a','agentRunId':'run-a','environmentId':'environment-a','executionMode':'perRun'})
  driver.opener=types.SimpleNamespace(open=Mock(return_value=io.BytesIO(''.join('data: '+json.dumps(item)+'\n\n' for item in items).encode())))
  return driver
 def test_public_historical_run_keeps_binding_tools_publication_and_successful_terminal(self):
  driver=self.driver(self.stream())
  self.assertTrue(driver.public_run_history({'agentRunId':'run-a','sessionId':'session-a'},'environment-a'))
  driver.http.assert_called_once_with('/api/sessions/session-a/agent-runs/run-a/environment')
 def test_missing_history_foreign_events_and_wrong_mode_cannot_claim_history_preserved(self):
  good=self.stream();foreign=copy.deepcopy(good);foreign[0]['event']['agentRunId']='other'
  for items in ([],good[:-1],good[:2]+good[3:],foreign):
   with self.subTest(items=items):
    with self.assertRaises(RuntimeError):self.driver(items).public_run_history({'agentRunId':'run-a','sessionId':'session-a'},'environment-a')
  driver=self.driver(good);driver.http.return_value['executionMode']='retained'
  with self.assertRaises(RuntimeError):driver.public_run_history({'agentRunId':'run-a','sessionId':'session-a'},'environment-a')
  driver.opener.open.assert_not_called()

if __name__=='__main__':unittest.main()
