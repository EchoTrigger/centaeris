"""Bounded native acceptance model using actual advertised tool calls only."""
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import json,os,ssl,uuid
from runsc_rotation_acceptance import next_rotation_call

def next_call(request,progress=None):return next_rotation_call(request,progress)

class Handler(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_POST(self):
        if self.path!='/v1/chat/completions':self.send_error(404);return
        size=int(self.headers.get('Content-Length','0'))
        if not 0<size<=1024*1024:self.send_error(413);return
        try:
            request=json.loads(self.rfile.read(size));call=next_call(request)
            if call and call[0] not in {tool['function']['name'] for tool in request.get('tools',[])}:
                raise ValueError('required actual tool is not advertised')
        except (RuntimeError,ValueError,KeyError,TypeError):self.send_error(422,'native acceptance model contract failed');return
        response_id='chatcmpl-'+uuid.uuid4().hex
        if call:
            name,arguments=call
            delta={'role':'assistant','tool_calls':[{'index':0,'id':'call-'+uuid.uuid4().hex,'type':'function','function':{'name':name,'arguments':json.dumps(arguments)}}]};reason='tool_calls'
        else:delta={'role':'assistant','content':'Native acceptance Run complete.'};reason='stop'
        streaming=request.get('stream',False)
        self.send_response(200);self.send_header('Content-Type','text/event-stream' if streaming else 'application/json');self.send_header('Connection','close');self.end_headers();self.close_connection=True
        if not streaming:
            message=dict(delta)
            if 'tool_calls' in message:message['tool_calls']=[{k:v for k,v in item.items() if k!='index'} for item in message['tool_calls']]
            self.wfile.write(json.dumps({'id':response_id,'object':'chat.completion','created':0,'model':'retained-acceptance','choices':[{'index':0,'message':message,'finish_reason':reason}],'usage':{'prompt_tokens':100,'completion_tokens':10,'total_tokens':110}}).encode());self.wfile.flush();return
        def emit(choices,usage=None):
            value={'id':response_id,'object':'chat.completion.chunk','model':'retained-acceptance','choices':choices}
            if usage is not None:value['usage']=usage
            self.wfile.write(('data: '+json.dumps(value)+'\n\n').encode())
        emit([{'index':0,'delta':delta,'finish_reason':None}]);emit([{'index':0,'delta':{},'finish_reason':reason}]);emit([],{'prompt_tokens':100,'completion_tokens':10,'total_tokens':110});self.wfile.write(b'data: [DONE]\n\n');self.wfile.flush()

if __name__=='__main__':
    server=ThreadingHTTPServer(('0.0.0.0',8080),Handler)
    if os.environ.get('MOCK_TLS_CERT') or os.environ.get('MOCK_TLS_KEY'):
        context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);context.load_cert_chain(os.environ['MOCK_TLS_CERT'],os.environ['MOCK_TLS_KEY']);server.socket=context.wrap_socket(server.socket,server_side=True)
    server.serve_forever()
