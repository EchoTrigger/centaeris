"""Pinned k6 observes the same real HTTP stream under short and sufficient deadlines."""
import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest

ROOT=Path(__file__).resolve().parents[2]
class RealK6StreamDeadlineTests(unittest.TestCase):
 def test_progress_stream_times_out_at_one_second_and_completes_at_three(self):
  binary=os.environ.get('CENTAERIS_K6_BINARY')
  if not binary or not Path(binary).is_file():self.skipTest('Set CENTAERIS_K6_BINARY to the pinned platform k6 binary')
  events=[{'type':kind,'payload':payload,'eventId':'event-'+str(i),'sequence':i+1,'sessionId':'probe-session','agentRunId':'probe-run'} for i,(kind,payload) in enumerate([
   ('tool_call',{'callId':'probe-call','toolName':'bash'}),('tool_result',{'callId':'probe-call','toolName':'bash','resultState':'successWithOutput'}),('agent_run_completed',{})])]
  producer=subprocess.run([sys.executable,str(ROOT/'perf/tests/session_stream_fixtures.py')],input=json.dumps(events),text=True,encoding='utf-8',capture_output=True,check=True,timeout=10)
  messages=[('data: '+json.dumps(item)+'\n\n').encode() for item in json.loads(producer.stdout)]
  class Handler(http.server.BaseHTTPRequestHandler):
   def do_GET(self):
    self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Connection','close');self.end_headers()
    try:
     for delay,message in zip((0,.25,1.25),messages):
      time.sleep(delay);self.wfile.write(message);self.wfile.flush()
    except (BrokenPipeError,ConnectionResetError,OSError):pass
   def log_message(self,*args):pass
  server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
  thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
  try:
   with tempfile.TemporaryDirectory() as directory:
    script=Path(directory)/'probe.js'
    script.write_text("import {Rate} from 'k6/metrics';\nimport {waitForTerminalObserved,verifyToolRun} from "+json.dumps((ROOT/'perf/k6/scenarios/common.js').as_uri())+";\nconst failed=new Rate('deadline_probe_failed');\nexport const options={vus:1,iterations:1,thresholds:{deadline_probe_failed:['rate<0.01']}};\nexport default function(){const started=Date.now();try{const result=waitForTerminalObserved({sessionid:'synthetic'},'', 'probe-session','probe-run',Number(__ENV.TEST_TIMEOUT));verifyToolRun(result,'probe-session','probe-run');console.log('DEADLINE_PROBE '+JSON.stringify({elapsedMs:Date.now()-started,firstResultMs:result.firstSuccessfulToolResultObservedAtMs-started}));failed.add(0);}catch(_){failed.add(1);}}\n",encoding='utf-8')
    for seconds in (1,3):
     with self.subTest(seconds=seconds):
      result=subprocess.run([binary,'--log-format','json','run',str(script)],env={**os.environ,'API_BASE':'http://127.0.0.1:'+str(server.server_port),'TEST_TIMEOUT':str(seconds),'K6_NO_USAGE_REPORT':'true'},capture_output=True,text=True,encoding='utf-8',timeout=10)
      if seconds==1:self.assertNotEqual(result.returncode,0,result.stdout+result.stderr)
      else:
       self.assertEqual(result.returncode,0,result.stdout+result.stderr)
       lines=[json.loads(line)['msg'] for line in result.stderr.splitlines() if line.startswith('{') and json.loads(line).get('msg','').startswith('DEADLINE_PROBE ')]
       self.assertEqual(len(lines),1,result.stdout+result.stderr)
       timing=json.loads(lines[0][len('DEADLINE_PROBE '):]);self.assertGreater(timing['elapsedMs'],1300)
       self.assertGreater(timing['firstResultMs'],100);self.assertLess(timing['firstResultMs'],timing['elapsedMs']-500)
  finally:
   server.shutdown();server.server_close();thread.join(timeout=3)

if __name__=='__main__':unittest.main()
