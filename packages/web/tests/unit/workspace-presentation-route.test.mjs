import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";
const harness = new URL("./componentHarness.mjs", import.meta.url).href;
const stub = code => `data:text/javascript;base64,${Buffer.from(code).toString("base64")}`;
const loader = subjectLoader({ extraOverrides: [
  ["react-router", stub(`export {matchPath} from ${JSON.stringify(import.meta.resolve("react-router"))}; import {context} from ${JSON.stringify(harness)}; export const Link=()=>null; export const useRouteLoaderData=id=>id==='authenticated'?{user:context.user}:{workspace:context.workspace,agents:context.agents}; export const useNavigate=()=>context.navigate;`)],
  ["../i18n", stub("export const t=key=>key; export const i18n={language:'en'}; export const useTranslation=()=>({t});")],
  ["react", stub(`export * from ${JSON.stringify(import.meta.resolve("react"))}; export {useState,useRef,useMemo,useCallback,useEffect,useLayoutEffect} from ${JSON.stringify(harness)}; export const useEffectEvent=callback=>callback; export const useSyncExternalStore=(_subscribe,snapshot)=>snapshot();`)],
  ["../preferences", stub("export const useEnterStartsNewLine=()=>false; export const readModelThinkingMode=()=>''; export const readPreferredModelIdentity=()=>''; export const writeModelThinkingMode=()=>{}; export const writePreferredModelIdentity=()=>{};")],
  ["../chat/transcriptTransport", stub(`import {context} from ${JSON.stringify(harness)}; export const createWorkspaceTranscriptTransport=()=>({loadTail:async()=>context.tail,loadPatches:async()=>{},loadActiveAgentRun:async()=>({agentRun:null})});`)],
  ["../chat/workspaceWebTransport", stub("export const streamWorkspaceAgentRun=async()=>{};")],
  ["../chat/workspacePresentationTransport", stub(`import {context} from ${JSON.stringify(harness)}; export const streamWorkspacePresentation=options=>context.presentation(options);`)],
  ...["../components/WorkspaceContextPanel", "../components/DocumentPreview", "../chat/WorkspaceComposer", "../chat/TranscriptBlockList", "../agent-chat/SessionAgentBreadcrumb", "../shell/ShellSidebar"].map(s => [s, stub(`export const ${s.split("/").at(-1)}=()=>null;`)]),
  ["../shell/HomePlane", stub("export const HomePlane=()=>null; export const HomeQuickActions=()=>null;")],
] });
const { AppPageContent } = await import(await loader(fileURLToPath(new URL("../../src/routes/AppRoute.jsx", import.meta.url))));
const row = changes => ({id:"session",workspaceId:"workspace",agentId:"agent",projectId:null,currentEnvironmentId:null,executionMode:null,title:"Synthetic",origin:"user",initialInputOrigin:null,status:"active",deletedAt:null,isPinned:false,isUnread:true,hasActiveAgentRun:false,updatedAt:"2026-10-23T00:00:00Z",latestAttentionSequence:"10",readThroughSequence:"0",attentionRevision:"1",isManuallyUnread:false,...changes});
const page = {schema:"transcript.page.v1",sessionId:"session",projectionVersion:"transcript.projection.v1",projectionGeneration:"generation",sourceHighWater:"10",blocks:[],olderCursor:null,hasOlder:false,resumeCursors:[{streamId:"workspace-transcript.v1",cursor:"10"}]};
const tick = () => new Promise(resolve => setImmediate(resolve));
async function setup(t, draft=false, navigationRead=null) {
  const previousWindow=globalThis.window,previousDocument=globalThis.document,previousStorage=globalThis.sessionStorage;
  const stored=new Map();globalThis.sessionStorage={getItem:key=>stored.get(key)??null,setItem:(key,value)=>stored.set(key,value),removeItem:key=>stored.delete(key)};
  globalThis.window={location:{pathname:"/w/workspace/agents/agent"},addEventListener(){},removeEventListener(){}};
  globalThis.document={hidden:false,addEventListener(){},removeEventListener(){}};
  t.after(()=>{globalThis.window=previousWindow;globalThis.document=previousDocument;globalThis.sessionStorage=previousStorage;});
  const calls=[]; let options;
  const env=environment({tail:page,request:async(path,request)=>{
    calls.push([path,request]);
    if(path==="/api/models") return {json:async()=>({models:[]})};
    if(path.includes("session-projects")) return {json:async()=>({projects:[]})};
    if(path.endsWith("/assets")) return {json:async()=>({assets:[]})};
    if(path.endsWith("/read")) return {session:row({isUnread:false,readThroughSequence:JSON.parse(request.body).throughSequence,attentionRevision:"2"})};
    if(path==="/api/sessions/session") return {json:async()=>({session:row()})};
    return navigationRead ? navigationRead() : {json:async()=>({sessions:[row()]})};
  },presentation:async(value)=>{
    options=value;
    value.onConnection("running");
    return new Promise(resolve=>value.signal.addEventListener("abort",resolve,{once:true}));
  }});
  const subject=renderer(AppPageContent,{agentId:"agent",workspaceDraft:draft,location:{search:draft?"?new=1":"?sessionId=session",state:null},modelsVersion:0},env);
  t.after(()=>subject.unmount());
  await subject.settle();
  const send=async(kind,body,cursor=null)=>{assert.ok(options,"the display subscription is active");await options.controller.acceptWithBackpressure({cursor,item:{schema:"workspace.presentation.v1",workspaceId:"workspace",agentId:"agent",sessionId:options.sessionId,kind,...(kind==="navigation"?{snapshotId:"navigation",offset:0,hasMore:false,...body}:body)}});await tick();await subject.settle();};
  return {subject,calls,send,getOptions:()=>options,rows:()=>nodes(subject.tree,n=>n.type.name==="SessionNavigationRow")};
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
