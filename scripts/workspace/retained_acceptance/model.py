"""Bounded deterministic local OpenAI completion model for real A/B execution.

Only the model is simulated: tool calls execute through the actual Runtime.

"""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import json

import uuid
import os
import ssl
import re
import hashlib

def content_text(message):

    content=message.get("content")

    if isinstance(content,str):

        return content

    if isinstance(content,list):

        return "\n".join(part.get("text","") for part in content if isinstance(part,dict))

    return ""

def find_task_id(value):

    if isinstance(value,str):

        try:

            return find_task_id(json.loads(value))

        except (ValueError,TypeError):

            # Tool messages can contain a human summary around their JSON.

            decoder=json.JSONDecoder()

            for index,character in enumerate(value):

                if character not in "[{":

                    continue

                try:

                    parsed,_=decoder.raw_decode(value,index)

                except ValueError:

                    continue

                found=find_task_id(parsed)

                if found:

                    return found

            return None

    if isinstance(value,dict):

        if isinstance(value.get("taskId"),str):

            return value["taskId"]

        for child in value.values():

            found=find_task_id(child)

            if found:

                return found

    if isinstance(value,list):

        for child in value:

            found=find_task_id(child)

            if found:

                return found

    return None

def long_command(index):
    return "printf '%%s\\n' %04d >> /mnt/data/retained-long-run-markers.txt; printf acceptance-long-%04d" % (index,index)


def next_call(request, progress=None):

    messages=request["messages"]

    last_user=max(index for index,message in enumerate(messages) if message.get("role")=="user")

    marker=content_text(messages[last_user])

    after=messages[last_user+1:]

    replies=[message for message in after if message.get("role")=="tool"]

    if "ACCEPTANCE_RUN_A" in marker:

        if replies:

            if find_task_id(content_text(replies[-1])) is None:

                raise ValueError("A did not receive a real background task id")

            return None

        return "background_task_start", {"program":"/bin/sh","args":["-c","printf 'retained acceptance file\\n' > /mnt/data/retained-acceptance.txt; printf 'Keep all acceptance commands bounded.\\n' > /mnt/data/AGENTS.md; exec sleep 14400"],"cwd":"/mnt/data","timeout_ms":14700000}

    if "ACCEPTANCE_HISTORY" in marker:
        if len(replies) < 70:
            if replies and "acceptance-history-ok" not in content_text(replies[-1]):
                raise ValueError("history command failed instead of completing")
            return "bash", {"command":"printf acceptance-history-ok"}
        if "acceptance-history-ok" not in content_text(replies[-1]):
            raise ValueError("last history command failed")
        return None

    if "ACCEPTANCE_LONG_RUN" in marker:
        count=0
        if replies:
            matches=set(re.findall(r'(?<![A-Za-z0-9_-])acceptance-long-(\d{4})(?!\d)',content_text(replies[-1])))
            if len(matches)!=1:
                raise ValueError('long Run command did not return its unique actual output')
            count=int(matches.pop())+1
        if not 0<=count<=1000:
            raise ValueError('long Run has an unexpected tool count')
        if progress is not None:
            if marker not in progress and len(progress)>=16:
                raise ValueError('bounded acceptance-model progress slots exhausted')
            if count<progress.get(marker,0):
                raise ValueError('long Run context lost progress; refusing to restart sequence')
            progress[marker]=count
        if count < 1000:
            return 'bash', {'command':long_command(count)}
        return None

    if "ACCEPTANCE_RUN_B" in marker:

        if len(replies)==0:

            return "background_task_list",{}

        if len(replies)==1:

            task_id=find_task_id(content_text(replies[-1]))

            if task_id is None:

                raise ValueError("B did not observe A's actual task")

            return "background_task_query",{"task_id":task_id,"cursor":0}

        if len(replies)==2:

            return "bash",{"command":"cat /mnt/data/retained-acceptance.txt"}

        if len(replies)==3:

            if "retained acceptance file" not in content_text(replies[-1]):

                raise ValueError("B failed to read the actual retained file")

            return None

    raise ValueError("unsupported acceptance prompt or unexpected tool round")

LONG_INSTRUCTIONS='Keep all acceptance commands bounded.'


def instructions_observation(request, call):
    if not call or call[0]!='bash' or 'acceptance-long-' not in call[1].get('command',''):
        return None
    if not any(LONG_INSTRUCTIONS in content_text(message) for message in request['messages']):
        raise ValueError('long Run model request lacks the actual AGENTS.md instructions')
    return {'kind':'automaticInstructionsObserved','commandIndex':int(call[1]['command'][-4:]),
        'instructionSha256':hashlib.sha256((LONG_INSTRUCTIONS+'\n').encode()).hexdigest()}


class Handler(BaseHTTPRequestHandler):

    progress={}  # Bounded fixture bookkeeping; authoritative acceptance uses Core events.

    def log_message(self,*args):

        pass

    def do_POST(self):

        if self.path != "/v1/chat/completions":

            self.send_error(404); return

        size=int(self.headers.get("Content-Length","0"))

        if not 0<size<=1024*1024:

            self.send_error(413); return

        try:

            request=json.loads(self.rfile.read(size))

            call=next_call(request,self.progress)
            observation=instructions_observation(request,call)
            if observation is not None:
                print(json.dumps(observation),flush=True)

            if call and call[0] not in {tool["function"]["name"] for tool in request.get("tools",[])}:

                raise ValueError("required actual tool is not advertised")

        except (ValueError,KeyError,TypeError):

            self.send_error(422,"acceptance model contract failed"); return

        response_id="chatcmpl-"+uuid.uuid4().hex

        if call:

            name,arguments=call

            delta={"role":"assistant","tool_calls":[{"index":0,"id":"call-"+uuid.uuid4().hex,"type":"function","function":{"name":name,"arguments":json.dumps(arguments)}}]}

            reason="tool_calls"

        else:

            delta={"role":"assistant","content":"Retained acceptance Run complete."}
            reason="stop"

        self.send_response(200)

        self.send_header("Content-Type","text/event-stream")

        self.send_header("Connection","close")

        self.end_headers()

        self.close_connection=True

        def emit(choice,usage=None):

            body={"id":response_id,"object":"chat.completion.chunk","model":"retained-acceptance","choices":choice}

            if usage is not None:

                body["usage"]=usage

            self.wfile.write(("data: "+json.dumps(body)+"\n\n").encode())

        emit([{"index":0,"delta":delta,"finish_reason":None}])

        emit([{"index":0,"delta":{},"finish_reason":reason}])

        emit([],{"prompt_tokens":100,"completion_tokens":10,"total_tokens":110})

        self.wfile.write(b"data: [DONE]\n\n")

        self.wfile.flush()

if __name__=="__main__":

    server=ThreadingHTTPServer(("0.0.0.0",8080),Handler)
    if os.environ.get("MOCK_TLS_CERT") or os.environ.get("MOCK_TLS_KEY"):
        context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(os.environ["MOCK_TLS_CERT"],os.environ["MOCK_TLS_KEY"])
        server.socket=context.wrap_socket(server.socket,server_side=True)
    server.serve_forever()

