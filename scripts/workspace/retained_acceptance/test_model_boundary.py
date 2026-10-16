import unittest
import model

NONCE='a'*32
MARKER='ACCEPTANCE_BOUNDARY_RUN steps=8 index=4 nonce='+NONCE+' timeout=180'
class BoundaryModelTests(unittest.TestCase):
 def request(self,replies=()):
  return {'messages':[{'role':'user','content':MARKER},{'role':'user','content':'# AGENTS.md instructions for /mnt/data\nKeep all acceptance commands bounded.'}]+list(replies)}
 def test_explicit_boundary_goal_generates_real_command(self):
  request=self.request();request['messages'][0],request['messages'][1]=request['messages'][1],request['messages'][0]
  name,args=model.next_call(request)
  self.assertEqual(name,'bash');self.assertIn('acceptance-boundary-'+NONCE+'-0000',args['command'])
 def test_boundary_command_waits_with_bounded_timeout_and_no_default_long_change(self):
  from boundary import parse_marker,command_for
  cfg=parse_marker(MARKER);command=command_for(cfg,4)
  self.assertIn('sleep 1',command);self.assertIn('exit 73',command)
  self.assertIn('call-boundary-'+NONCE+'-0004',command)
  self.assertNotIn('sleep',command_for(cfg,3))
  self.assertEqual(model.long_command(4),"printf '%s\\n' 0004 >> /mnt/data/retained-long-run-markers.txt; printf acceptance-long-0004")
 def test_steps_and_identity_fail_loudly(self):
  from boundary import parse_marker
  for text in (MARKER.replace('steps=8','steps=7'),MARKER.replace('steps=8','steps=33'),MARKER.replace('index=4','index=8'),MARKER.replace(NONCE,'bad'),MARKER+' extra'):
   with self.subTest(text=text),self.assertRaises(ValueError):parse_marker(text)
 def test_progress_uses_last_actual_output_not_number_of_replies(self):
  request={'messages':[{'role':'user','content':'Keep all acceptance commands bounded.'},{'role':'user','content':MARKER},{'role':'tool','content':'acceptance-boundary-'+NONCE+'-0003'}]}
  name,args=model.next_call(request)
  self.assertIn('call-boundary-'+NONCE+'-0004',args['command']);self.assertEqual(args['timeout_ms'],210000)
 def test_missing_instruction_or_wrong_output_refused(self):
  for messages in ([{'role':'user','content':MARKER}], [{'role':'user','content':'Keep all acceptance commands bounded.'},{'role':'user','content':MARKER},{'role':'tool','content':'acceptance-boundary-'+'b'*32+'-0003'}]):
   with self.assertRaises(ValueError):model.next_call({'messages':messages})
if __name__=='__main__':unittest.main()

class ActualBoundaryHttpTests(unittest.TestCase):
 def test_real_http_response_binds_deterministic_call_to_wait_command(self):
  import json,threading
  from http.server import ThreadingHTTPServer
  from urllib.request import Request,ProxyHandler,build_opener
  request={'messages':[{'role':'user','content':'Keep all acceptance commands bounded.'},{'role':'user','content':MARKER},{'role':'tool','content':'acceptance-boundary-'+NONCE+'-0003'}], 'tools':[{'type':'function','function':{'name':'bash'}}]}
  server=ThreadingHTTPServer(('127.0.0.1',0),model.Handler);thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
  try:
   req=Request('http://127.0.0.1:'+str(server.server_port)+'/v1/chat/completions',data=json.dumps(request).encode(),headers={'Content-Type':'application/json'})
   with build_opener(ProxyHandler({})).open(req,timeout=5) as response:body=json.load(response)
   call=body['choices'][0]['message']['tool_calls'][0]
   self.assertEqual(call['id'],'call-boundary-'+NONCE+'-0004')
   self.assertIn(call['id'],json.loads(call['function']['arguments'])['command'])
  finally:server.shutdown();server.server_close();thread.join(timeout=5)
