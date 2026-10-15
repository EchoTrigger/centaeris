"""Read actual API DB, Docker task service and retained file for A/B assertions.

This is an observer, not an authority issuer: the root relay is used only to

query with the already persisted controller fence, including one stale query.

"""

import argparse

import json

from pathlib import Path

import subprocess

import uuid


def stale_execute_fence(previous):
    task_id=previous['backgroundTaskIds'][0]
    task=next(task for task in previous['taskService']['result']['tasks'] if task['taskId']==task_id)
    epoch_text, opaque=task['operationId'].split(':',1)
    if not epoch_text.isdigit() or int(epoch_text)<=0 or not opaque:
        raise RuntimeError('original task did not expose a canonical operation identity')
    return {**previous['fence'],'controllerEpoch':int(epoch_text),'controllerPurpose':'execute'}


def stale_operation_id(fence, operation):
    return str(fence['controllerEpoch'])+':acceptance-stale-'+operation+'-'+uuid.uuid4().hex

def task_rpc(container_id,fence,operation="list",**arguments):

    payload={"schema":"environment.task.v2","request":{"operation":operation,"fence":fence,**arguments}}

    result=subprocess.run(["docker","exec","-i","--user","0:0",container_id,"/opt/centaeris/bin/execution_agent","environment-task-rpc"],input=json.dumps(payload),check=True,capture_output=True,text=True)

    response=json.loads(result.stdout)

    if response.get("schema")!="environment.task.v2":

        raise RuntimeError("task service response schema invalid")

    return response


def main():

    parser=argparse.ArgumentParser()

    parser.add_argument("--compose", required=True, type=Path)

    parser.add_argument("--run", required=True)

    parser.add_argument("--file",default="retained-acceptance.txt")

    parser.add_argument("--result",required=True,type=Path)

    parser.add_argument("--previous",type=Path)

    args=parser.parse_args()

    if args.file.startswith("/") or any(part in ("", ".", "..") for part in args.file.split("/")):

        raise SystemExit("file must be a safe relative workspace path")

    code='''

import json

from django.conf import settings

from django.db import connection

from app_core.models import AgentRun, AgentRunEnvironmentRelease

assert settings.DATABASES['default']['NAME']=='retained_acceptance'

run=AgentRun.objects.select_related('environment').get(id=RUN)

assert run.status=='completed', 'Run is not actually completed'

release=AgentRunEnvironmentRelease.objects.get(agent_run=run)

env=run.environment

assert env.controller_run_id==run.id and env.controller_purpose=='cleanup' and env.controller_released

with connection.cursor() as c:

 c.execute('SELECT DISTINCT instance_id FROM runtime.execution_instances WHERE agent_run_id=%s AND environment_id=%s',[run.id,env.id])

 instances=[row[0] for row in c.fetchall()]

 assert len(instances)==1, 'Run must have one actual retained instance'

 c.execute('SELECT container_id,memory_bytes,cpu_milli,pids,workspace_bytes,host_id FROM runtime.resident_sandboxes WHERE environment_id=%s AND state=%s',[env.id,'resident'])

 residents=c.fetchall()

 assert len(residents)==1, 'resident resources must count the physical instance exactly once'

 assert residents[0][1:5]==(268435456,1000,128,67108864), 'declared resource budget mismatch'

 c.execute('SELECT instance_id,cleanup_complete FROM runtime.agent_run_cleanup_receipts WHERE agent_run_id=%s',[run.id])

 receipt=c.fetchone()

 assert receipt==(instances[0],True), 'no durable real cleanup receipt'

print(json.dumps({'agentRunId':run.id,'sessionId':run.session_id,'environmentId':env.id,'instanceId':instances[0],

 'containerId':residents[0][0],'resources':residents[0][1:5], 'hostId':residents[0][5], 'releaseReceiptId':release.id,

 'fence':{'environmentInstanceId':instances[0],'controllerId':run.id,'controllerEpoch':env.controller_epoch,'controllerPurpose':'cleanup'}}))

'''.replace("RUN",repr(args.run))

    cmd=["docker","compose","-f",str(args.compose),"exec","-T","api","/app/.venv/bin/python","manage.py","shell","-c",code]

    raw=subprocess.run(cmd,check=True,capture_output=True,text=True).stdout

    # Django shell may print its standard auto-import banner.

    state=json.loads(next(line for line in reversed(raw.splitlines()) if line.startswith("{")))

    def rpc(fence,operation="list",**arguments):
        return task_rpc(state["containerId"],fence,operation,**arguments)

    tasks=rpc(state["fence"])

    if tasks.get("status") != "success":

        raise RuntimeError("current persisted cleanup fence could not query actual task service")

    listed=tasks["result"]

    if listed.get("kind") != "listed":

        raise RuntimeError("actual task service returned a different result")

    backgrounds=[task for task in listed["tasks"] if task["scope"]=={"kind":"environment"}]

    if not backgrounds or any(task["state"]!="running" or task["cleanupComplete"] for task in backgrounds):

        raise RuntimeError("environment background task did not remain active after Run cleanup")

    for task in listed["tasks"]:

        if task["scope"]=={"kind":"run","runId":state["agentRunId"]} and not task["cleanupComplete"]:

            raise RuntimeError("Run-scoped task cleanup incomplete")

    state["backgroundTaskIds"]=sorted(task["taskId"] for task in backgrounds)

    state["taskService"]=tasks
    sizes=subprocess.run(['docker','exec','--user','0:0',state['containerId'],'/bin/sh','-c',
        'wc -c < /run/centaeris/task-control/journal.json; du -sb /run/centaeris/task-output'],
        check=True,capture_output=True,text=True).stdout.splitlines()
    state['retention']={'journalBytes':int(sizes[0]),'outputDirectoryBytes':int(sizes[1].split()[0]),
        'activeTasks':sum(task['state']!='exited' for task in listed['tasks']),
        'terminalTasks':sum(task['state']=='exited' for task in listed['tasks'])}

    digest=subprocess.run(["docker","exec",state["containerId"],"sha256sum","/mnt/data/" + args.file],check=True,capture_output=True,text=True).stdout.split()[0]

    state["fileSha256"]=digest
    runtime_id=subprocess.run(["docker","compose","-f",str(args.compose),"ps","-q","runtime"],check=True,capture_output=True,text=True).stdout.strip()
    if not runtime_id:
        raise RuntimeError("dedicated Runtime container is missing")
    runtime_state=json.loads(subprocess.run(["docker","inspect",runtime_id,"--format","{{json .State}}"],check=True,capture_output=True,text=True).stdout)
    if not runtime_state["Running"]:
        raise RuntimeError("dedicated Runtime is not running")
    state["runtimeStartup"]={"containerId":runtime_id,"startedAt":runtime_state["StartedAt"],"hostPid":runtime_state["Pid"]}

    if args.previous:

        previous=json.loads(args.previous.read_text(encoding="utf-8"))

        for key in ("sessionId","environmentId","instanceId","containerId","fileSha256","resources","backgroundTaskIds"):

            if state[key] != previous[key]:

                raise RuntimeError(f"A to B changed retained {key}")

        if (state["runtimeStartup"]["startedAt"]==previous["runtimeStartup"]["startedAt"]
                or state["runtimeStartup"]["hostPid"]==previous["runtimeStartup"]["hostPid"]):
            raise RuntimeError("Runtime did not actually restart between A and B")
        if state["fence"]["controllerEpoch"] <= previous["fence"]["controllerEpoch"]:

            raise RuntimeError("B did not advance controller epoch")

        stale=rpc(previous["fence"])

        if stale.get("status") != "error" or stale.get("error") != "task_controller_fenced":

            raise RuntimeError("old controller was not explicitly fenced at actual task service")

        old_execute=stale_execute_fence(previous)
        stale_mutation=rpc(old_execute,"terminate",taskId=state["backgroundTaskIds"][0],operationId=stale_operation_id(old_execute,'terminate'))

        if stale_mutation.get("status")!="error" or stale_mutation.get("error")!="task_controller_fenced":

            raise RuntimeError("old controller was not fenced from terminating the real background task")

        current_query=rpc(state["fence"],"query",taskId=state["backgroundTaskIds"][0],cursor=0,maxBytes=65536)

        if (current_query.get("status")!="success" or current_query["result"].get("kind")!="task"

                or current_query["result"]["task"]["taskId"]!=state["backgroundTaskIds"][0]

                or current_query["result"]["task"]["state"]!="running"

                or current_query["result"]["task"]["cleanupComplete"]):

            raise RuntimeError("background task was not still running after the stale termination attempt")

        state["oldControllerRejected"]=True

        state["staleTerminationRejected"]=True

        state["currentQueryAfterStaleTermination"]=current_query
        for operation, arguments in [
            ('start', {'operationId':stale_operation_id(old_execute,'start'), 'scope':{'kind':'environment'},
                'program':'/bin/true', 'args':[], 'cwd':'/mnt/data','timeoutMs':1000}),
            ('input', {'taskId':state['backgroundTaskIds'][0], 'operationId':stale_operation_id(old_execute,'input'),
                'data':[120], 'close':False}),
        ]:
            rejected=rpc(old_execute, operation, **arguments)
            if rejected.get('status')!='error' or rejected.get('error')!='task_controller_fenced':
                raise RuntimeError('obsolete controller mutation accepted: '+operation)
            state['stale'+operation.title()+'Result']=rejected

    args.result.write_text(json.dumps(state,indent=2),encoding="utf-8")

    print(json.dumps({"agentRunId":state["agentRunId"],"instanceId":state["instanceId"],"residentCount":1,"oldControllerRejected":state.get("oldControllerRejected",False)}))

if __name__ == "__main__":

    main()

