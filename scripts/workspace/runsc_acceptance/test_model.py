import importlib.util,json,sys,threading,unittest,urllib.request,urllib.error
from http.server import ThreadingHTTPServer
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
spec=importlib.util.spec_from_file_location('native_model',Path(__file__).with_name('model.py'));model=importlib.util.module_from_spec(spec);spec.loader.exec_module(model)
class NativeModelTests(unittest.TestCase):
 def setUp(self):
  self.server=ThreadingHTTPServer(('127.0.0.1',0),model.Handler);self.thread=threading.Thread(target=self.server.serve_forever,daemon=True);self.thread.start();self.url='http://127.0.0.1:'+str(self.server.server_address[1])+'/v1/chat/completions'
 def tearDown(self):self.server.shutdown();self.server.server_close();self.thread.join(timeout=3)
 def request(self,payload):
  req=urllib.request.Request(self.url,data=json.dumps(payload).encode(),headers={'Content-Type':'application/json'})
  return urllib.request.build_opener(urllib.request.ProxyHandler({})).open(req,timeout=3)
 def test_initial_oracle_uses_advertised_background_tool(self):
  payload={'messages':[{'role':'user','content':'RUNSC_ROTATION '+'a'*32+' A 0'}],'tools':[{'function':{'name':'background_task_start'}}],'stream':False}
  with self.request(payload) as response:value=json.load(response)
  call=value['choices'][0]['message']['tool_calls'][0]
  self.assertEqual(call['function']['name'],'background_task_start');self.assertEqual(json.loads(call['function']['arguments'])['program'],'python3')
 def test_later_holder_run_streams_actual_task_start(self):
  payload={'messages':[{'role':'user','content':'RUNSC_ROTATION '+'a'*32+' A 2'}],'tools':[{'function':{'name':'background_task_start'}}],'stream':True}
  with self.request(payload) as response:body=response.read().decode()
  records=[line[6:] for line in body.splitlines() if line.startswith('data: ')]
  self.assertEqual(records[-1],'[DONE]');first=json.loads(records[0]);self.assertEqual(first['choices'][0]['delta']['tool_calls'][0]['function']['name'],'background_task_start')
 def test_failed_actual_tool_result_returns_bounded_contract_error(self):
  payload={'messages':[{'role':'user','content':'RUNSC_ROTATION_BLOCK '+'a'*32+' A'},{'role':'tool','content':json.dumps({'schema':'background_task.result.v1','state':'unknown','taskId':'owned'})}],'tools':[]}
  with self.assertRaises(urllib.error.HTTPError) as context:self.request(payload)
  self.assertEqual(context.exception.code,422)
 def test_unadvertised_tool_is_rejected_before_response(self):
  payload={'messages':[{'role':'user','content':'RUNSC_ROTATION '+'a'*32+' A 0'}],'tools':[{'function':{'name':'bash'}}]}
  with self.assertRaises(urllib.error.HTTPError) as context:self.request(payload)
  self.assertEqual(context.exception.code,422)
if __name__=='__main__':unittest.main()
