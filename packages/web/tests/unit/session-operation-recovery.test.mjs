import assert from "node:assert/strict";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { ApiError, environment, nodes, renderer, subjectLoader } from "./componentHarness.mjs";
const harness = new URL("./componentHarness.mjs", import.meta.url).href;
const stub = code => `data:text/javascript;base64,${Buffer.from(code).toString("base64")}`;
const loader = subjectLoader({ extraOverrides: [
  ["../api.ts", stub(`import {context,ApiError} from ${JSON.stringify(harness)}; export {ApiError}; export const apiJson=(...args)=>context.request(...args); export const apiResponse=(...args)=>context.response(...args);`)],
  ["../i18n", stub("export const t=key=>key; export const i18n={language:'en'}; export const useTranslation=()=>({t});")],
  ["react-router", stub(`export const matchPath=()=>null; import {context} from ${JSON.stringify(harness)}; export const Link=()=>null; export const useRouteLoaderData=id=>id==='authenticated'?{user:context.user}:{workspace:context.workspace,agents:context.agents}; export const useNavigate=()=>context.navigate;`)],
  ["react", stub(`export * from ${JSON.stringify(import.meta.resolve("react"))}; export {useState,useRef,useMemo,useCallback,useEffect,useLayoutEffect} from ${JSON.stringify(harness)}; export const useEffectEvent=callback=>callback; export const useSyncExternalStore=(_subscribe,snapshot)=>snapshot();`)],

  ["../preferences", stub("export const useEnterStartsNewLine=()=>false; export const readModelThinkingMode=()=>''; export const readPreferredModelIdentity=()=>''; export const writeModelThinkingMode=()=>{}; export const writePreferredModelIdentity=()=>{};")],
  ["../chat/transcriptTransport", stub(`import {context} from ${JSON.stringify(harness)}; export const createWorkspaceTranscriptTransport=()=>({loadTail:async id=>context.tail(id),loadPatches:async()=>{},loadActiveAgentRun:async()=>({agentRun:context.activeRun??null})});`)],
  ["../chat/workspaceWebTransport", stub(`import {context} from ${JSON.stringify(harness)}; export const streamWorkspaceAgentRun=()=>context.stream();`)],
  ...["../components/WorkspaceContextPanel", "../components/DocumentPreview", "../chat/WorkspaceComposer", "../chat/TranscriptBlockList", "../agent-chat/SessionAgentBreadcrumb", "../shell/ShellSidebar"].map(s => [s, stub(`export const ${s.split("/").at(-1)}=()=>null;`)]),
  ["../shell/HomePlane", stub("export const HomePlane=()=>null; export const HomeQuickActions=()=>null;")],
] });
const { AppPageContent } = await import(await loader(fileURLToPath(new URL("../../src/routes/AppRoute.jsx", import.meta.url))));

const pendingKey = 'centaeris.pendingOperation.v1:["user","workspace","submitMessage"]';
const composer = subject => nodes(subject.tree, n => n.type.name === "WorkspaceComposer")[0].props;
const inputBody = options => options.body instanceof FormData ? Object.fromEntries(options.body) : JSON.parse(options.body);
const receipt = id => ({ operationId:id, command:"submitMessage", status:"accepted", sessionId:"work", agentRunId:"run", turnId:"turn" });
function fixture(context, request) {
  const previousWindow=globalThis.window,previousStorage=globalThis.sessionStorage;context.after(()=>{globalThis.window=previousWindow;globalThis.sessionStorage=previousStorage;});
  const data=new Map(); globalThis.sessionStorage={getItem:k=>data.get(k)??null,setItem:(k,v)=>data.set(k,v),removeItem:k=>data.delete(k)};
  const calls=[];
  globalThis.window={location:{pathname:"/w/workspace/app"},addEventListener(){},removeEventListener(){}};
  const env=environment({tail:async()=>({schema:"transcript.page.v1",sessionId:"work",projectionVersion:"transcript.projection.v1",projectionGeneration:"generation",sourceHighWater:"1",blocks:[],olderCursor:null,hasOlder:false,resumeCursors:[{streamId:"workspace-transcript.v1",cursor:"1"}]}),request:async(path,options)=>{
    calls.push([path,options]); let value;
    if(path==="/api/models") value={models:[{id:"model",name:"Model",provider:"test",modelName:"test"},{id:"other",name:"Other",provider:"test",modelName:"other"}]};
    else if(path.includes("/operations/")||options?.method==="POST") return request(path,options);
    else if(path.includes("session-projects")) value={projects:[]};
    else if(path.startsWith("/api/workspaces/")&&path.includes("/sessions")) value={sessions:[{id:"work",agentId:"agent",title:"work",hasActiveAgentRun:false,status:"active"}]};
    else if(path.endsWith("/assets")) value={assets:[]};
    else value={session:{id:"work",workspaceId:"workspace",agentId:"agent",projectId:null,title:"work",status:"active",hasActiveAgentRun:false,initialInputOrigin:null}};
    return {...value,json:async()=>value};
  }});
  env.response=async(...args)=>{const value=await env.request(...args);return {...value,json:async()=>value};};
  const props={agentId:"agent",workspaceDraft:true,location:{search:"",state:null},modelsVersion:0,onSessionAccepted:()=>{}};
  return {data,calls,props,subject:renderer(AppPageContent,props,env)};
}
async function send(subject,text) {composer(subject).onDraftChange(text);await subject.settle();await composer(subject).onSubmit({preventDefault(){}});await new Promise(resolve=>setTimeout(resolve,20));await subject.settle();}
for(const change of ["text","model","attachment"]) test(`ordinary submit corrects unaccepted ${change} with the original operation identity`,async context=>{
  let posts=0;const f=fixture(context,async(path,options)=>{
    if(!options) throw new ApiError("operation_not_found",404);
    posts++;if(posts===1) throw new TypeError("offline");return receipt(inputBody(options).operationId);
  });
  try {await f.subject.settle();await send(f.subject,"original");const id=JSON.parse(f.data.get(pendingKey)).operationId;
    if(change==="model") {composer(f.subject).onModelIdChange("other");await f.subject.settle();}
    if(change==="attachment") {await composer(f.subject).onUploadAttachment({currentTarget:{files:[new File(["content"],"edited.txt",{type:"text/plain"})],value:""}});await f.subject.settle();}
    await send(f.subject,change==="text"?"edited":"original");assert.equal(posts,2);
    const body=inputBody(f.calls.filter(([,o])=>o?.method==="POST").at(-1)[1]);assert.equal(body.operationId,id);assert.equal(body.text,change==="text"?"edited":"original");if(change==="attachment")assert.equal(body.files.name,"edited.txt");
  }finally{f.subject.unmount();}
});
for(const outcome of ["accepted","conflict","offline"])test(`changed resubmit preserves draft when original ${outcome}`,async context=>{
 let posts=0,lookups=0;const f=fixture(context,async(path,options)=>{
   if(!options){lookups++;if(lookups===1||outcome==="conflict"&&lookups===2)throw new ApiError("operation_not_found",404);if(outcome==="offline")throw new TypeError("offline");return receipt(JSON.parse(f.data.get(pendingKey)).operationId);}
   posts++;if(posts===1)throw new TypeError("offline");throw new ApiError("operation_conflict",409);
 });
 try{await f.subject.settle();await send(f.subject,"original");const id=JSON.parse(f.data.get(pendingKey)).operationId;await send(f.subject,"edited");
 assert.equal(composer(f.subject).draft,"edited");assert.equal(JSON.parse(f.data.get(pendingKey)).operationId,id);assert.equal(posts,outcome==="conflict"?2:1);if(outcome!=="offline")assert.equal(nodes(f.subject.tree,n=>n.type==="a"&&n.props.children==="operations.reviewSeparate").length,1);
 }finally{f.subject.unmount();}
});

test("modified submit with an already accepted receipt restores review while keeping the new draft",async context=>{
 const f=fixture(context,async(_path,options)=>{assert.equal(options,undefined);return receipt("old");});
 f.data.set(pendingKey,JSON.stringify({operationId:"old",fingerprint:"a".repeat(64),assetIds:[],inputChanged:false,receipt:receipt("old")}));
 try{await f.subject.settle();await send(f.subject,"edited");assert.equal(composer(f.subject).draft,"edited");assert.equal(f.calls.filter(([,o])=>o?.method==="POST").length,0);assert.equal(nodes(f.subject.tree,n=>n.type==="a"&&n.props.children==="operations.reviewSeparate").length,1);}
 finally{f.subject.unmount();}
});

test("accepted original in the current Session restores history without consuming the edited draft",async context=>{
 const f=fixture(context,async(_path,options)=>{assert.equal(options,undefined);return receipt("old");});
 f.props.workspaceDraft=false;f.props.location={search:"?sessionId=work",state:null};
 try{await f.subject.settle();f.data.set(pendingKey,JSON.stringify({operationId:"old",fingerprint:"a".repeat(64),assetIds:[],inputChanged:false,receipt:receipt("old")}));await send(f.subject,"edited");assert.equal(composer(f.subject).draft,"edited");assert.equal(f.calls.filter(([,o])=>o?.method==="POST").length,0);assert.equal(nodes(f.subject.tree,n=>n.props.role==="alert")[0].props.children[0],"operations.inputUnconfirmed");assert.equal(f.data.has(pendingKey),false);}
 finally{f.subject.unmount();}
});
