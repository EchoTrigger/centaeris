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


def update_background_references(value, task_ids):
    if isinstance(value,dict):
        identifier=value.get('taskId')
        if isinstance(identifier,str):
            if value.get('state')=='exited' and value.get('cleanupComplete') is True:
                task_ids.discard(identifier)
            else:
                task_ids.add(identifier)
        for child in value.values():
            update_background_references(child,task_ids)
    elif isinstance(value,list):
        for child in value:
            update_background_references(child,task_ids)


def compaction_summary(request):
    """Summarize the real Core JSONL contract, never fixture progress bookkeeping."""
    messages=request['messages']
    if not any(content_text(message).startswith('[model_compaction_prompt_v1]') for message in messages):
        return None
    if (len(messages)!=1 or messages[0].get('role')!='user' or request.get('tools')
            or request.get('tool_choice','none')!='none'):
        raise ValueError('unsupported compaction request shape')
    match=re.fullmatch(r'\[model_compaction_prompt_v1\].*?<previous-summary>\n(.*?)\n</previous-summary>\s*<conversation>\n(.*)\n</conversation>\s*',
        content_text(messages[0]),re.DOTALL)
    if match is None:
        raise ValueError('malformed Core compaction prompt')
    previous,conversation=match.groups()
    records=[json.loads(line) for line in conversation.splitlines() if line.strip()]
    if any(not isinstance(row,dict) or row.get('role') not in ('system','user','assistant','tool')
            or not isinstance(row.get('content'),str) for row in records):
        raise ValueError('malformed Core conversation record')
    goals=[row['content'] for row in records if row['role']=='user'][-1:]
    if not goals:
        goals=re.findall(r'^Goal: ([^\n]+)$',previous,re.MULTILINE)
    if len(goals)!=1 or len(goals[0])>1024:
        raise ValueError('compaction lacks unique bounded acceptance goal')
    goal=re.fullmatch(r'(ACCEPTANCE_RUN_A|ACCEPTANCE_LONG_RUN)(?:[ \t]+[^\n]+)?',goals[0])
    if goal is None or len(re.findall(r'ACCEPTANCE_[A-Z_]+',goals[0]))!=1:
        raise ValueError('compaction has unsupported or ambiguous acceptance goal')
    long_run=goal[1]=='ACCEPTANCE_LONG_RUN'
    prior=re.findall(r'^Last successful output: `(acceptance-long-\d{4})`\.$',previous,re.MULTILINE)
    last=prior[-1] if prior else None
    failed=int(re.search(r'^Failed tool results: (\d+)\.$',previous,re.MULTILINE).group(1)) if re.search(
        r'^Failed tool results: (\d+)\.$',previous,re.MULTILINE) else 0
    last_failure=re.findall(r'^Last failure: (.+)$',previous,re.MULTILINE)
    last_failure=last_failure[-1] if last_failure else None
    pending={}
    requested_files=set(re.findall(r'^Requested file setup: `([^`\n]+)` \(tool arguments; existence not inferred\)\.$',previous,re.MULTILINE))
    task_ids=set(re.findall(r'^Observed taskId: `([^`\n]+)`\.$',previous,re.MULTILINE))
    for identifier,name in re.findall(r'^Pending tool: `([^`\n]+)` \(`([^`\n]+)`\)\.$',previous,re.MULTILINE):
        pending[identifier]=name
    for row in records:
        if row['role']=='assistant':
            for call in row.get('toolCalls',[]):
                pending[call['toolCallId']]=call['name']
                if call['name']=='background_task_start':
                    arguments=json.loads(call['argumentsJson'])
                    for path in ('/mnt/data/retained-acceptance.txt','/mnt/data/AGENTS.md'):
                        if path in json.dumps(arguments):requested_files.add(path)
        if row['role']!='tool':
            continue
        pending.pop(row['toolCallId'],None)
        if row['status']!='ok' or row['resultState'] not in ('success_with_output','success_without_output'):
            failed+=1
            last_failure=json.dumps(row['content'][:384],ensure_ascii=True)
            continue
        if row['name'] in ('background_task_start','background_task_query','background_task_list','background_task_stop'):
            # Ordinary terminal bash task IDs are historical storage facts, not
            # live continuation references. Preserve only the background domain.
            update_background_references(json.loads(row['content']),task_ids)
        outputs=set(re.findall(r'(?<![A-Za-z0-9_-])acceptance-long-(\d{4})(?!\d)',row['content']))
        if outputs:
            if not long_run or row['name']!='bash' or len(outputs)!=1:
                raise ValueError('ambiguous successful command output')
            index=int(outputs.pop())
            if index>=1000 or (last is not None and index<=int(last[-4:])):
                raise ValueError('compaction output position regressed')
            last='acceptance-long-%04d' % index
    lines=['## Goal','Goal: '+goals[0],
        ('Complete exactly 1000 bounded commands; preserve the retained background task and files.' if long_run
         else 'Preserve the requested retained background task and its observed identity; retain the requested file setup.'),
        '## Progress']
    if last:
        lines.append('Last successful output: `'+last+'`.')
    else:
        lines.append('No successful numbered command output was supplied.')
    lines.append('Failed tool results: %d.' % failed)
    if last_failure:
        lines.append('Last failure: '+last_failure)
    lines.extend('Observed taskId: `'+identifier+'`.' for identifier in sorted(task_ids))
    lines.extend('Requested file setup: `'+path+'` (tool arguments; existence not inferred).' for path in sorted(requested_files))
    lines.extend('Pending tool: `'+identifier+'` (`'+name+'`).' for identifier,name in sorted(pending.items()))
    lines.extend(['## Next steps',
        ('Continue from actual numbered tool results; never replay completed commands. Resolve pending calls and failures before proceeding.' if long_run
         else 'Continue from the supplied background task observations; resolve pending calls and failures before proceeding.'),
        'Keep all acceptance commands bounded. Do not terminate the retained background task.'])
    summary='\n'.join(lines)
    if len(summary)>4096:
        raise ValueError('bounded fixture summary capacity exceeded')
    return summary


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
        else:
            # Core may retain no tool suffix. Its accepted summary is then the
            # durable observation; the in-memory progress map never supplies it.
            summaries=[content_text(message) for message in messages[:last_user]
                if content_text(message).startswith('## Goal\nGoal: '+marker+'\n')]
            if summaries:
                outputs=re.findall(r'^Last successful output: `acceptance-long-(\d{4})`\.$',
                    summaries[-1],re.MULTILINE)
                if len(outputs)!=1:
                    raise ValueError('compacted long Run lacks unique actual output')
                count=int(outputs[0])+1
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

            summary=compaction_summary(request)
            call=None if summary is not None else next_call(request,self.progress)
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

            delta={"role":"assistant","content":summary if summary is not None else "Retained acceptance Run complete."}
            reason="stop"

        self.send_response(200)

        streaming=request.get('stream',False)
        self.send_header("Content-Type","text/event-stream" if streaming else "application/json")

        self.send_header("Connection","close")

        self.end_headers()

        self.close_connection=True

        if not streaming:
            message=dict(delta)
            if 'tool_calls' in message:
                message['tool_calls']=[{key:value for key,value in item.items() if key!='index'}
                    for item in message['tool_calls']]
            body={'id':response_id,'object':'chat.completion','created':0,'model':'retained-acceptance',
                'choices':[{'index':0,'message':message,'finish_reason':reason}],
                'usage':{'prompt_tokens':100,'completion_tokens':10,'total_tokens':110}}
            self.wfile.write(json.dumps(body).encode())
            self.wfile.flush()
            return

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

