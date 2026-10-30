import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { ApiError, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";
const harness = new URL("./componentHarness.mjs", import.meta.url).href;
const stub = code => `data:text/javascript;base64,${Buffer.from(code).toString("base64")}`;
const loader = subjectLoader({ extraOverrides: [
  ["../api.ts", stub(`import {context} from ${JSON.stringify(harness)}; export {ApiError} from ${JSON.stringify(harness)}; export const apiJson=(...args)=>context.request(...args); export const apiResponse=(...args)=>context.request(...args);`)],
  ["react-router", stub(`export {matchPath} from ${JSON.stringify(import.meta.resolve("react-router"))}; import {context} from ${JSON.stringify(harness)}; export const Link=()=>null; export const useRouteLoaderData=id=>id==='authenticated'?{user:context.user}:{workspace:context.workspace,agents:context.agents}; export const useNavigate=()=>context.navigate;`)],
  ["../i18n", stub("export const t=key=>key; export const i18n={language:'en'}; export const useTranslation=()=>({t});")],
  ["react", stub(`export * from ${JSON.stringify(import.meta.resolve("react"))}; export {useState,useRef,useMemo,useCallback,useEffect,useLayoutEffect} from ${JSON.stringify(harness)}; export const useEffectEvent=callback=>callback; export const useSyncExternalStore=(_subscribe,snapshot)=>snapshot();`)],
  ["../preferences", stub("export const useEnterStartsNewLine=()=>false; export const readModelThinkingMode=()=>''; export const readPreferredModelIdentity=()=>''; export const writeModelThinkingMode=()=>{}; export const writePreferredModelIdentity=()=>{};")],
  ["../chat/transcriptTransport", stub(`import {context} from ${JSON.stringify(harness)}; export const createWorkspaceTranscriptTransport=()=>({loadTail:async()=>context.tail,loadPatches:async()=>{},loadActiveAgentRun:async(...args)=>context.activeRun?context.activeRun(...args):({agentRun:null})});`)],
  ["../chat/workspaceWebTransport", stub("export const streamWorkspaceAgentRun=async()=>{};")],
  ["../chat/workspacePresentationTransport", stub(`import {context} from ${JSON.stringify(harness)}; export const streamWorkspacePresentation=options=>context.presentation(options);`)],
  ...["../components/WorkspaceContextPanel", "../components/DocumentPreview", "../chat/WorkspaceComposer", "../chat/TranscriptBlockList", "../agent-chat/SessionAgentBreadcrumb", "../shell/ShellSidebar"].map(s => [s, stub(`export const ${s.split("/").at(-1)}=()=>null;`)]),
  ["../shell/HomePlane", stub("export const HomePlane=()=>null; export const HomeQuickActions=()=>null;")],
] });
const { AppPageContent } = await import(await loader(fileURLToPath(new URL("../../src/routes/AppRoute.jsx", import.meta.url))));
const row = changes => ({id:"session",workspaceId:"workspace",agentId:"agent",projectId:null,currentEnvironmentId:null,executionMode:null,title:"Synthetic",origin:"user",initialInputOrigin:null,status:"active",deletedAt:null,isPinned:false,isUnread:true,hasActiveAgentRun:false,updatedAt:"2026-10-23T00:00:00Z",latestAttentionSequence:"10",readThroughSequence:"0",attentionRevision:"1",isManuallyUnread:false,...changes});
const page = {schema:"transcript.page.v1",sessionId:"session",projectionVersion:"transcript.projection.v1",projectionGeneration:"generation",sourceHighWater:"10",blocks:[],olderCursor:null,hasOlder:false,resumeCursors:[{streamId:"workspace-transcript.v1",cursor:"10"}]};
const tick = () => new Promise(resolve => setImmediate(resolve));
async function setup(t, draft=false, navigationRead=null, controls={}) {
  const previousWindow=globalThis.window,previousDocument=globalThis.document,previousStorage=globalThis.sessionStorage;
  const stored=new Map();globalThis.sessionStorage={getItem:key=>stored.get(key)??null,setItem:(key,value)=>stored.set(key,value),removeItem:key=>stored.delete(key)};
  globalThis.window={location:{pathname:"/w/workspace/agents/agent"},requestAnimationFrame(){},addEventListener(){},removeEventListener(){}};
  globalThis.document={hidden:false,addEventListener(){},removeEventListener(){}};
  t.after(()=>{globalThis.window=previousWindow;globalThis.document=previousDocument;globalThis.sessionStorage=previousStorage;});
  const calls=[]; let options; let serverRun=null;
  const env=environment({tail:page,activeRun:controls.activeRun || (async()=>({agentRun:serverRun})),request:async(path,request)=>{
    calls.push([path,request]);
    const override = await controls.request?.(path,request);
    if (override !== undefined) return override;
    if(path==="/api/models?workspaceId=workspace") return {json:async()=>({models:controls.models ?? [{id:"model",providerId:"provider",modelName:"model",thinkingModes:[]}]})};
    if(path.includes("session-projects")) return {json:async()=>({projects:[]})};
    if(path.endsWith("/assets")) return {json:async()=>({assets:[]})};
    if(path.endsWith("/read")) return {session:row({isUnread:false,readThroughSequence:JSON.parse(request.body).throughSequence,attentionRevision:"2"})};
    if(path==="/api/sessions/session") return {json:async()=>({session:row()})};
    return navigationRead ? navigationRead() : {json:async()=>({sessions:controls.sessions || [row()]})};
  },presentation:async(value)=>{
    options=value;
    controls.connections = (controls.connections || 0) + 1;
    if (controls.presentation) return controls.presentation(value);
    value.onConnection("running");
    return new Promise(resolve=>value.signal.addEventListener("abort",resolve,{once:true}));
  }});
  const props={agentId:"agent",workspaceDraft:draft,location:{search:draft?"?new=1":"?sessionId=session",state:null},modelsVersion:0,onSessionAccepted:path=>controls.accepted?.(path)};
  const subject=renderer(AppPageContent,props,env);
  t.after(()=>subject.unmount());
  await subject.settle();
  const send=async(kind,body,cursor=null)=>{if(kind==="checkpoint") serverRun=body.agentRun && ["queued","running"].includes(body.agentRun.status) ? {...body.agentRun,streamCursor:cursor || "current"} : null;assert.ok(options,"the display subscription is active");await options.controller.acceptWithBackpressure({cursor,item:{schema:"workspace.presentation.v1",workspaceId:"workspace",agentId:"agent",sessionId:options.sessionId,kind,...(kind==="navigation"?{snapshotId:"navigation",offset:0,hasMore:false,...body}:body)}});await tick();await subject.settle();};
  return {subject,calls,send,props,getOptions:()=>options,rows:()=>nodes(subject.tree,n=>n.type.name==="SessionNavigationRow")};
}

test("a fresh chat keeps a workspace subscription for background notifications",async t=>{
  const f=await setup(t,true);
  assert.ok(f.getOptions(),"navigation remains push-driven when no run is open");
});

test("a visible completed conversation clears its dot after applied delivery without navigation",async t=>{
  const f=await setup(t);
  await f.send("snapshot",{page});
  await f.send("navigation",{sessions:[row()]});
  await f.send("checkpoint",{projectionGeneration:"generation",sourceHighWater:"10",agentRun:null},"applied-10");
  await tick();await f.subject.settle();
  const reads=f.calls.filter(([path])=>path.endsWith("/read"));
  assert.equal(reads.length,1);
  assert.equal(JSON.parse(reads[0][1].body).throughSequence,"10");
  const sidebar=nodes(f.subject.tree,n=>n.type.name==="ShellSidebar")[0];
  assert.equal(sidebar.props.sessionProps.sessions[0].isUnread,false);
});

test("a late startup HTTP list cannot replace a newer pushed navigation snapshot", async t => {
  let release;
  const f = await setup(t, true, () => new Promise(resolve => { release = resolve; }));
  const latest = [row({id: "new-session", title: "New background completion"})];
  await f.send("navigation", {sessions: latest});
  release({json: async () => ({sessions: [row()]})});
  await tick(); await f.subject.settle();
  const sidebar = nodes(f.subject.tree, n => n.type.name === "ShellSidebar")[0];
  assert.deepEqual(sidebar.props.sessionProps.sessions.map(row => row.id), ["new-session"]);
});


test("pushed navigation opens the requested conversation while the startup HTTP list is still pending", async t => {
  let release;
  const f = await setup(t, false, () => new Promise(resolve => { release = resolve; }));
  assert.equal(f.getOptions().sessionId, null);
  await f.send("navigation", {sessions: [row()]});
  assert.equal(f.getOptions().sessionId, "session");
  await f.send("snapshot", {page});
  await f.send("checkpoint", {projectionGeneration:"generation", sourceHighWater:"10", agentRun:null}, "applied-10");
  assert.equal(nodes(f.subject.tree,n=>n.type.name==="TranscriptBlockList")[0].props.loadingHistory, false);
  release({json:async()=>({sessions:[row()]})});
  await tick(); await f.subject.settle();
  assert.equal(f.getOptions().sessionId, "session");
});

const composer = f => nodes(f.subject.tree,n=>n.type.name==="WorkspaceComposer")[0].props;
const checkpoint = (agentRun, sourceHighWater="10") => ({projectionGeneration:"generation",sourceHighWater,agentRun});
const run = (agentRunId="run-new",status="queued") => ({agentRunId,status,startedAt:null,completedAt:["completed","failed","cancelled"].includes(status)?"2026-10-23T00:00:01Z":null});
const receipt = request => ({operationId:JSON.parse(request.body).operationId,command:"submitMessage",status:"accepted",sessionId:"session",agentRunId:"run-new",turnId:"turn-new"});
async function settleUntil(f, ready) {
  for(let i=0;i<100;i++){
    await tick();await f.subject.settle();
    if(ready()) return;
  }
  assert.fail("the controlled request did not reach its expected boundary");
}
async function startSend(f, ready=()=>!composer(f).sending) {
  if(composer(f).loadingHistory) await f.send("checkpoint",checkpoint(null),"ready");
  composer(f).onDraftChange("synthetic input");await f.subject.settle();
  composer(f).onSubmit({preventDefault(){}});
  await settleUntil(f,ready);
}

test("an accepted Run can be stopped before its Session read or first checkpoint returns",async t=>{
  let releaseRead; let submitted=false;
  const f=await setup(t,false,null,{request:async(path,request)=>{
    if(path.endsWith("/messages")){submitted=true;return receipt(request);}
    if(path==="/api/sessions/session" && submitted) return new Promise(resolve=>{releaseRead=resolve;});
    if(path.endsWith("/cancel")) return {json:async()=>({agentRunId:"run-new",status:"queued",disposition:"requested"})};
  }});
  await f.send("snapshot",{page});
  await startSend(f,()=>Boolean(releaseRead));
  t.after(()=>releaseRead?.({json:async()=>({session:row()})}));
  assert.equal(composer(f).activeAgentRunId,"run-new");
  assert.equal(composer(f).hasActiveAgentRun,true);
  await composer(f).onCancelActiveAgentRun(); await f.subject.settle();
  assert.equal(f.calls.filter(([path])=>path==="/api/sessions/session/agent-runs/run-new/cancel").length,1);
  assert.equal(composer(f).activeAgentRunId,"run-new","requested is not terminal");
  releaseRead({json:async()=>({session:row()})});await tick();await f.subject.settle();
});

test("old checkpoints cannot discard an accepted target but matching terminal can",async t=>{
  let submitted=false;
  const f=await setup(t,false,null,{
    activeRun:async()=>({agentRun:submitted?{agentRunId:"run-new",status:"queued",streamCursor:"current"}:null}),
    request:async(path,request)=>{if(path.endsWith("/messages")){submitted=true;return receipt(request);}},
  });
  await f.send("snapshot",{page});await startSend(f);
  await f.send("checkpoint",checkpoint(null),"old-null");
  assert.equal(composer(f).activeAgentRunId,"run-new");
  await f.send("checkpoint",checkpoint(run("old-run","cancelled")),"old-terminal");
  assert.equal(composer(f).activeAgentRunId,"run-new");
  await f.send("checkpoint",checkpoint(run()),"queued");
  assert.equal(composer(f).activeAgentRunId,"run-new");
  await f.send("checkpoint",checkpoint(run("run-new","cancelled")),"terminal");
  assert.equal(composer(f).hasActiveAgentRun,false);
});

test("a terminal checkpoint arriving before a receipt cannot resurrect the Run",async t=>{
  let release;
  const f=await setup(t,false,null,{request:async(path,request)=>path.endsWith("/messages")?new Promise(resolve=>{release=()=>resolve(receipt(request));}):undefined});
  await f.send("snapshot",{page});await startSend(f,()=>Boolean(release));
  await f.send("checkpoint",checkpoint(run("run-new","cancelled")),"terminal-first");
  release();await settleUntil(f,()=>!composer(f).sending);
  assert.equal(composer(f).sending,false);
  assert.ok(f.calls.filter(([path])=>path==="/api/sessions/session").length>=2);
  assert.equal(composer(f).hasActiveAgentRun,false);
});

test("an already terminal cancellation refreshes presentation without reporting failure",async t=>{
  const controls={request:async(path)=>path.endsWith("/cancel")?{json:async()=>({agentRunId:"run-new",status:"cancelled",disposition:"terminal"})}:undefined};
  const f=await setup(t,false,null,controls);
  await f.send("snapshot",{page});await f.send("checkpoint",checkpoint(run()),"queued");
  const before=controls.connections;
  await composer(f).onCancelActiveAgentRun();await f.subject.settle();
  assert.equal(nodes(f.subject.tree,n=>n.props.role==="alert").length,0);
  assert.ok(controls.connections>before,"terminal acknowledgement requests fresh presentation");
  assert.equal(composer(f).hasActiveAgentRun,false);
});

test("current queued Run is cancellable without waiting for presentation",async t=>{
  const f=await setup(t,false,null,{activeRun:async()=>({agentRun:{agentRunId:"queued-run",status:"queued",streamCursor:"cursor"}})});
  assert.equal(composer(f).activeAgentRunId,"queued-run");
  assert.equal(composer(f).hasActiveAgentRun,true);
});

test("a late active-Run read cannot overwrite a newer checkpoint",async t=>{
  let release;
  const f=await setup(t,false,null,{activeRun:()=>new Promise(resolve=>{release=resolve;})});
  await f.send("snapshot",{page});await f.send("checkpoint",checkpoint(run()),"queued");
  release({agentRun:null});await tick();await f.subject.settle();
  assert.equal(composer(f).activeAgentRunId,"run-new");
});


test("new Session acceptance exposes Stop before its follow-up read and preserves it across routing",async t=>{
  let releaseRead;let acceptedPath;let submitted=false;
  const f=await setup(t,true,null,{
    accepted:path=>{acceptedPath=path;},
    activeRun:async()=>({agentRun:{agentRunId:"run-new",status:"queued",streamCursor:"cursor"}}),
    request:async(path,request)=>{
      if(path.endsWith("/messages")){submitted=true;return receipt(request);}
      if(path==="/api/sessions/session" && submitted) return new Promise(resolve=>{releaseRead=resolve;});
      if(path.endsWith("/cancel")) return {json:async()=>({agentRunId:"run-new",status:"queued",disposition:"requested"})};
    },
  });
  await startSend(f,()=>Boolean(releaseRead));
  assert.equal(composer(f).sessionId,"");
  assert.equal(composer(f).activeAgentRunId,"run-new");
  await composer(f).onCancelActiveAgentRun();
  assert.equal(f.calls.filter(([path])=>path==="/api/sessions/session/agent-runs/run-new/cancel").length,1);
  releaseRead({json:async()=>({session:row()})});await tick();await f.subject.settle();
  assert.ok(acceptedPath.endsWith("/sessions/session"));
  f.props.workspaceDraft=false;f.props.location.search="?sessionId=session";
  await f.subject.settle();
  assert.equal(composer(f).activeAgentRunId,"run-new");
});

for(const outcome of ["terminal","failure"]){
  test(`a late ${outcome} cancellation from another Session cannot affect the current request`,async t=>{
    let finishOld;let finishNew;
    const f=await setup(t,false,null,{sessions:[row(),row({id:"other"})],request:async(path)=>{
      if(path==="/api/sessions/other")return {json:async()=>({session:row({id:"other"})})};
      if(path.endsWith("/run-new/cancel"))return new Promise((resolve,reject)=>{finishOld=()=>outcome==="failure"?reject(new Error("old request failed")):resolve({json:async()=>({agentRunId:"run-new",status:"cancelled",disposition:"terminal"})});});
      if(path.endsWith("/other-run/cancel"))return new Promise(resolve=>{finishNew=()=>resolve({json:async()=>({agentRunId:"other-run",status:"running",disposition:"requested"})});});
    }});
    await f.send("snapshot",{page});await f.send("checkpoint",checkpoint(run()),"old-run");
    const old=composer(f).onCancelActiveAgentRun();await tick();
    f.props.location.search="?sessionId=other";await f.subject.settle();
    await f.send("snapshot",{page:{...page,sessionId:"other"}});
    await f.send("checkpoint",checkpoint(run("other-run","running")),"new-run");
    const current=composer(f).onCancelActiveAgentRun();await tick();await f.subject.settle();
    assert.equal(composer(f).cancellingAgentRunId,"other-run");
    finishOld();await old;await f.subject.settle();
    assert.equal(composer(f).activeAgentRunId,"other-run");
    assert.equal(composer(f).cancellingAgentRunId,"other-run");
    assert.equal(nodes(f.subject.tree,n=>n.props.role==="alert").length,0);
    finishNew();await current;await f.subject.settle();
    assert.equal(composer(f).cancellingAgentRunId,"");
    assert.equal(composer(f).activeAgentRunId,"other-run");
  });
}

test("a lost submit response keeps Stop available while recovering the original operation",async t=>{
  let release;let accepted;
  const f=await setup(t,false,null,{request:async(path,request)=>{
    if(path.endsWith("/messages")){accepted=receipt(request);throw new TypeError("lost acknowledgement");}
    if(path.includes("/operations/submitMessage/"))return new Promise(resolve=>{release=()=>resolve(accepted);});
  }});
  await f.send("snapshot",{page});await startSend(f,()=>Boolean(release));
  assert.equal(composer(f).hasActiveAgentRun,true);
  assert.equal(composer(f).activeAgentRunId,"");
  await composer(f).onCancelActiveAgentRun();
  assert.equal(f.calls.filter(([path])=>path.endsWith("/cancel")).length,0);
  release();await settleUntil(f,()=>!composer(f).sending);
  assert.equal(composer(f).activeAgentRunId,"run-new");
  assert.equal(f.calls.filter(([path])=>path.endsWith("/messages")).length,1);
  assert.equal(f.calls.filter(([path])=>path.endsWith(`/operations/submitMessage/${accepted.operationId}`)).length,1);
});

test("a late permission denial cannot discard a newer known cancellation target",async t=>{
  let rejectRead;
  const f=await setup(t,false,null,{activeRun:()=>new Promise((_resolve,reject)=>{rejectRead=reject;})});
  await f.send("snapshot",{page});await f.send("checkpoint",checkpoint(run()),"queued");
  assert.equal(composer(f).hasActiveAgentRun,true);
  rejectRead(new ApiError("forbidden",403));await tick();await f.subject.settle();
  assert.equal(composer(f).activeAgentRunId,"run-new");
  assert.equal(nodes(f.subject.tree,n=>n.props.role==="alert").length,0);
});


test("a current presentation permission denial clears the cancellation target",async t=>{
  let deny;
  const f=await setup(t,false,null,{presentation:()=>new Promise((_resolve,reject)=>{deny=()=>reject(new ApiError("forbidden",403));})});
  await f.send("snapshot",{page});await f.send("checkpoint",checkpoint(run()),"queued");
  deny();await tick();await f.subject.settle();
  assert.equal(composer(f).hasActiveAgentRun,false);
});

test("an accepted target converges when presentation skips directly to a later Run",async t=>{
  let submitted=false;
  const f=await setup(t,false,null,{
    activeRun:async()=>({agentRun:submitted?{agentRunId:"later-run",status:"running",streamCursor:"later"}:null}),
    request:async(path,request)=>{if(path.endsWith("/messages")){submitted=true;return receipt(request);}},
  });
  await f.send("snapshot",{page});await startSend(f);
  assert.equal(composer(f).activeAgentRunId,"run-new");
  await f.send("checkpoint",checkpoint(run("later-run","running")),"later");
  assert.equal(composer(f).activeAgentRunId,"later-run");
});

test("a late cancel failure cannot contaminate a newer Run in the same Session",async t=>{
  let fail;
  const f=await setup(t,false,null,{request:async(path)=>path.endsWith("/cancel")?new Promise((_resolve,reject)=>{fail=()=>reject(new Error("old cancel failed"));}):undefined});
  await f.send("snapshot",{page});await f.send("checkpoint",checkpoint(run()),"queued");
  const old=composer(f).onCancelActiveAgentRun();await tick();
  await f.send("checkpoint",checkpoint(run("run-new","cancelled")),"terminal");
  await f.send("checkpoint",checkpoint(run("next-run","running")),"next");
  assert.equal(composer(f).cancellingAgentRunId,"");
  fail();await old;await f.subject.settle();
  assert.equal(composer(f).activeAgentRunId,"next-run");
  assert.equal(nodes(f.subject.tree,n=>n.props.role==="alert").length,0);
});


for(const current of [null,"run-new"]){
  test(`repeated stale checkpoints cannot overwrite a current Run read of ${current}`,async t=>{
    let submitted=false;
    const f=await setup(t,false,null,{
      activeRun:async()=>({agentRun:submitted && current?{agentRunId:current,status:"queued",streamCursor:"current"}:null}),
      request:async(path,request)=>{if(path.endsWith("/messages")){submitted=true;return receipt(request);}},
    });
    await f.send("snapshot",{page});await startSend(f);
    await f.send("checkpoint",checkpoint(run("older-run","running")),"older-1");
    assert.equal(composer(f).activeAgentRunId,current || "");
    await f.send("checkpoint",checkpoint(run("older-run","running")),"older-2");
    assert.equal(composer(f).activeAgentRunId,current || "");
  });
}

test("a stale empty checkpoint after receipt handoff cannot remove a currently queued Run",async t=>{
  let submitted=false;
  const f=await setup(t,false,null,{
    activeRun:async()=>({agentRun:submitted?{agentRunId:"run-new",status:"queued",streamCursor:"current"}:null}),
    request:async(path,request)=>{if(path.endsWith("/messages")){submitted=true;return receipt(request);}},
  });
  await f.send("snapshot",{page});await startSend(f);
  await f.send("checkpoint",checkpoint(run()),"matching");
  await f.send("checkpoint",checkpoint(null),"stale-empty");
  assert.equal(composer(f).activeAgentRunId,"run-new");
});


const transcript = f => nodes(f.subject.tree,n=>n.type.name==="TranscriptBlockList")[0].props;

test("Stop is available immediately and cancels the original receipt after acceptance arrives",async t=>{
  let accept;let sent;
  const f=await setup(t,false,null,{request:async(path,request)=>{
    if(path.endsWith("/messages")){sent=request;return new Promise(resolve=>{accept=()=>resolve(receipt(request));});}
    if(path.endsWith("/cancel"))return {json:async()=>({agentRunId:"run-new",status:"queued",disposition:"requested"})};
  }});
  await f.send("snapshot",{page});await startSend(f,()=>Boolean(accept));
  assert.equal(composer(f).hasActiveAgentRun,true);
  const stopping=composer(f).onCancelActiveAgentRun();await tick();await f.subject.settle();
  assert.equal(composer(f).hasActiveAgentRun,true);
  assert.equal(f.calls.filter(([path])=>path.endsWith("/cancel")).length,0);
  accept();await stopping;await settleUntil(f,()=>f.calls.some(([path])=>path.endsWith("/cancel")));
  assert.equal(f.calls.filter(([path])=>path.endsWith("/messages")).length,1);
  assert.equal(f.calls.filter(([path])=>path==="/api/sessions/session/agent-runs/run-new/cancel").length,1);
  assert.ok(JSON.parse(sent.body).operationId);
});

for(const status of ["failed","cancelled","completed"]){
 test(`matching ${status} settles pending input even when no Core user block exists`,async t=>{
  const f=await setup(t,false,null,{request:async(path,request)=>path.endsWith("/messages")?receipt(request):undefined});
  await f.send("snapshot",{page});await startSend(f);
  await f.send("checkpoint",checkpoint(run("run-new",status)),"terminal-without-event");
  assert.equal(transcript(f).pendingUserMessage.text,"synthetic input");
  assert.equal(transcript(f).pendingUserMessage.status,status);
  assert.equal(typeof transcript(f).pendingUserMessage.completedAtMs,"number");
  assert.equal(transcript(f).running,false);
  assert.equal(composer(f).hasActiveAgentRun,false);
 });
}

test("terminal before receipt settles the matching submission without reactivating Stop",async t=>{
 let accept;
 const f=await setup(t,false,null,{request:async(path,request)=>path.endsWith("/messages")?new Promise(resolve=>{accept=()=>resolve(receipt(request));}):undefined});
 await f.send("snapshot",{page});await startSend(f,()=>Boolean(accept));
 await f.send("checkpoint",checkpoint(run("run-new","failed")),"failed-first");
 accept();await settleUntil(f,()=>!composer(f).sending);
 assert.equal(transcript(f).pendingUserMessage.status,"failed");
 assert.equal(composer(f).hasActiveAgentRun,false);
});

test("empty input and missing model preserve the draft without dispatching a request",async t=>{
 const f=await setup(t,false,null,{models:[]});
 await f.send("snapshot",{page});await f.send("checkpoint",checkpoint(null),"ready");
 await composer(f).onSubmit({preventDefault(){}});await f.subject.settle();
 assert.equal(composer(f).hasActiveAgentRun,false);
 composer(f).onDraftChange("keep this draft");await f.subject.settle();
 await composer(f).onSubmit({preventDefault(){}});await f.subject.settle();
 assert.equal(composer(f).draft,"keep this draft");
 assert.equal(composer(f).hasActiveAgentRun,false);
 assert.equal(f.calls.filter(([path])=>path.endsWith("/messages")).length,0);
});

test("repeated Stop while cancellation is pending stays actionable and shares one request",async t=>{
 let finish;
 const f=await setup(t,false,null,{request:async(path)=>path.endsWith("/cancel")?new Promise(resolve=>{finish=()=>resolve({json:async()=>({agentRunId:"run-new",status:"running",disposition:"requested"})});}):undefined});
 await f.send("snapshot",{page});await f.send("checkpoint",checkpoint(run()),"active");
 const first=composer(f).onCancelActiveAgentRun();await tick();await f.subject.settle();
 const second=composer(f).onCancelActiveAgentRun();await tick();await f.subject.settle();
 assert.equal(composer(f).hasActiveAgentRun,true);
 assert.equal(f.calls.filter(([path])=>path.endsWith("/cancel")).length,1);
 finish();await Promise.all([first,second]);await f.subject.settle();
 assert.equal(composer(f).hasActiveAgentRun,true,"requested does not declare terminal");
});


test("a new Session keeps its pending text visible while acceptance metadata is delayed",async t=>{
 let read;let submitted=false;
 const f=await setup(t,true,null,{request:async(path,request)=>{
  if(path.endsWith("/messages")){submitted=true;return receipt(request);}
  if(path==="/api/sessions/session"&&submitted)return new Promise(resolve=>{read=()=>resolve({json:async()=>({session:row()})});});
 }});
 await startSend(f,()=>Boolean(read));
 assert.equal(transcript(f).pendingUserMessage.sessionId,null,"the pending text belongs to the still-visible draft conversation");
 assert.equal(transcript(f).pendingUserMessage.agentRunId,"run-new");
 read();await settleUntil(f,()=>!composer(f).sending);
});

test("terminal releases local send progress even when the follow-up Session read is still pending",async t=>{
 let read;let submitted=false;
 const f=await setup(t,false,null,{request:async(path,request)=>{
  if(path.endsWith("/messages")){submitted=true;return receipt(request);}
  if(path==="/api/sessions/session"&&submitted)return new Promise(resolve=>{read=()=>resolve({json:async()=>({session:row()})});});
 }});
 await f.send("snapshot",{page});await startSend(f,()=>Boolean(read));
 await f.send("checkpoint",checkpoint(run("run-new","failed")),"failed-before-metadata");
 assert.equal(composer(f).sending,false);
 assert.equal(transcript(f).pendingUserMessage.status,"failed");
 read();await tick();await f.subject.settle();
});


test("terminal without a new snapshot retires its receipt so a new submission can create a new operation",async t=>{
 let posts=0;const ids=[];let finishMetadata;
 const f=await setup(t,false,null,{request:async(path,request)=>{
  if(path.endsWith("/messages")){posts++;ids.push(JSON.parse(request.body).operationId);return {...receipt(request),agentRunId:posts===1?"run-new":"run-retry",turnId:posts===1?"turn-new":"turn-retry"};}
  if(path==="/api/sessions/session"&&posts===1)return new Promise(resolve=>{finishMetadata=()=>resolve({json:async()=>({session:row()})});});
 }});
 await f.send("snapshot",{page});await startSend(f,()=>Boolean(finishMetadata));
 t.after(()=>finishMetadata?.());
 await f.send("checkpoint",checkpoint(run("run-new","failed")),"failed-no-snapshot");
 assert.equal(sessionStorage.getItem('centaeris.pendingOperation.v1:["user","workspace","submitMessage"]'),null);
 composer(f).onDraftChange("synthetic input");await f.subject.settle();
 assert.equal(composer(f).draft,"synthetic input");
 composer(f).onSubmit({preventDefault(){}});await settleUntil(f,()=>posts===2&&!composer(f).sending);
 assert.notEqual(ids[0],ids[1]);
 assert.equal(composer(f).activeAgentRunId,"run-retry");
});


test("a new Session terminal before its details still preserves the accepted navigation",async t=>{
 let read;let submitted=false;let acceptedPath;
 const f=await setup(t,true,null,{accepted:path=>{acceptedPath=path;},request:async(path,request)=>{
  if(path.endsWith("/messages")){submitted=true;return receipt(request);}
  if(path==="/api/sessions/session"&&submitted)return new Promise(resolve=>{read=()=>resolve({json:async()=>({session:row()})});});
  if(path.endsWith("/cancel"))return {json:async()=>({agentRunId:"run-new",status:"cancelled",disposition:"terminal"})};
 }});
 await startSend(f,()=>Boolean(read));
 await composer(f).onCancelActiveAgentRun();await f.subject.settle();
 assert.equal(transcript(f).pendingUserMessage.status,"cancelled");
 assert.equal(transcript(f).pendingUserMessage.sessionId,null);
 read();await settleUntil(f,()=>Boolean(acceptedPath));
 assert.ok(acceptedPath.endsWith("/sessions/session"));
 assert.equal(transcript(f).pendingUserMessage.sessionId,"session");
 assert.equal(transcript(f).pendingUserMessage.status,"cancelled");
});


test("checking a stopped unknown submission cancels the recovered original Run without reposting",async t=>{
 let available=false;let original;let lookups=0;
 const f=await setup(t,false,null,{request:async(path,request)=>{
  if(path.endsWith("/messages")){original=request;throw new TypeError("lost acknowledgement");}
  if(path.includes("/operations/submitMessage/")){lookups++;if(!available)throw new ApiError("operation_not_found",404);return receipt(original);}
  if(path.endsWith("/cancel"))return {json:async()=>({agentRunId:"run-new",status:"queued",disposition:"requested"})};
 }});
 await f.send("snapshot",{page});await startSend(f);
 await composer(f).onCancelActiveAgentRun();await f.subject.settle();
 assert.ok(lookups>=2);available=true;
 const retry=nodes(f.subject.tree,n=>n.type==="button"&&n.props.children==="agentChat.retry")[0];
 assert.ok(retry);retry.props.onClick();
 await settleUntil(f,()=>!composer(f).sending);
 assert.equal(f.calls.filter(([path])=>path==="/api/sessions/session/agent-runs/run-new/cancel").length,1);
 assert.equal(f.calls.filter(([path])=>path.endsWith("/messages")).length,1);
});

test("a new conversation pending input never appears in another Session while its Stop survives",async t=>{
 let accept;
 const f=await setup(t,true,null,{sessions:[row(),row({id:"other"})],request:async(path,request)=>{
  if(path.endsWith("/messages"))return new Promise(resolve=>{accept=()=>resolve(receipt(request));});
  if(path==="/api/sessions/other")return {json:async()=>({session:row({id:"other"})})};
  if(path.endsWith("/cancel"))return {json:async()=>({agentRunId:"run-new",status:"queued",disposition:"requested"})};
 }});
 await startSend(f,()=>Boolean(accept));await composer(f).onCancelActiveAgentRun();
 f.props.workspaceDraft=false;f.props.location.search="?sessionId=other";await f.subject.settle();
 assert.equal(transcript(f).sessionId,"other");
 assert.equal(transcript(f).pendingUserMessage,null);
 accept();await settleUntil(f,()=>f.calls.some(([path])=>path.endsWith("/cancel")));
 assert.equal(transcript(f).pendingUserMessage,null);
 assert.equal(f.calls.filter(([path])=>path==="/api/sessions/session/agent-runs/run-new/cancel").length,1);
});


test("Stop during file preparation restores the draft immediately and late file bytes never dispatch",async t=>{
 let finishRead;let readFinished=false;
 const file=new File(["attachment"],"note.txt",{type:"text/plain"});
 Object.defineProperty(file,"arrayBuffer",{value:()=>readFinished?Promise.resolve(new ArrayBuffer(0)):new Promise(resolve=>{finishRead=()=>{readFinished=true;resolve(new ArrayBuffer(0));};})});
 const f=await setup(t,true,null,{request:async(path,request)=>path.endsWith("/messages")?receipt(request):undefined});
 await composer(f).onUploadAttachment({currentTarget:{files:[file],value:""}});await f.subject.settle();
 await startSend(f,()=>Boolean(finishRead));
 await composer(f).onCancelActiveAgentRun();await settleUntil(f,()=>!composer(f).sending);
 assert.equal(composer(f).draft,"synthetic input");
 assert.equal(composer(f).hasActiveAgentRun,false);
 assert.equal(transcript(f).pendingUserMessage,null);
 assert.equal(f.calls.filter(([path])=>path.endsWith("/messages")).length,0);
 finishRead();await tick();await f.subject.settle();
 assert.equal(f.calls.filter(([path])=>path.endsWith("/messages")).length,0);
 assert.equal(composer(f).hasActiveAgentRun,false);
});


test("editing a new draft during supplemental input keeps it after the old response",async t=>{
 let finish;let requests=0;
 const f=await setup(t,false,null,{request:async(path,request)=>{
  if(path.endsWith("/supplements")){requests++;const input=JSON.parse(request.body);return new Promise(resolve=>{finish=()=>resolve({json:async()=>({sessionId:"session",agentRunId:"run-new",supplementId:input.supplementId,disposition:"accepted",queuedCount:1})});});}
 }});
 await f.send("snapshot",{page});await f.send("checkpoint",checkpoint(run()),"active");
 composer(f).onDraftChange("extra note");await f.subject.settle();
 composer(f).onSubmit({preventDefault(){}});await settleUntil(f,()=>Boolean(finish));
 composer(f).onDraftChange("next draft");await f.subject.settle();
 composer(f).onSubmit({preventDefault(){}});await f.subject.settle();
 assert.equal(requests,1);assert.equal(composer(f).draft,"next draft");
 finish();await settleUntil(f,()=>!composer(f).sending);
 assert.equal(composer(f).draft,"next draft");
});
