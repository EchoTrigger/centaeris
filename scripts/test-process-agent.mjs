// Native isolated-profile acceptance. Only the loopback mock receives model calls.
import { createRuntimeHostTransport } from "../packages/desktop/src/runtimeHostTransport.mjs";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import http from "node:http";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
const repo=path.resolve(path.dirname(fileURLToPath(import.meta.url)),"..");
const root=await fs.mkdtemp(path.join(os.tmpdir(),"centaeris-process-agent-"));
const profile=path.join(root,"profile");
const requests=[];
const events=[];
let stage=0;
let processId;
let held;
const mock=http.createServer((req,res)=>{
  let body="";req.setEncoding("utf8");req.on("data",c=>body+=c);
  req.on("end",()=>{
    const payload=JSON.parse(body);requests.push(payload);
    let delta={content:"Background command started."}, finish="stop";
    if(stage===0){
      delta={tool_calls:[{index:0,id:"process-call-1",type:"function",function:{name:"process_start",arguments:JSON.stringify({program:"bash",args:["-c","sleep 2; printf background-complete"],timeout_ms:10000})}}]};finish="tool_calls";
    } else if(stage===2){
      assert(body.includes("automatic notification, not a user request"),"completion must reach the model");
      const text=JSON.stringify(payload.messages);
      processId=text.match(/process-[0-9a-f]{32}/)?.[0];
      assert(processId,"completion contains a process reference");
      delta={tool_calls:[{index:0,id:"process-read-1",type:"function",function:{name:"process_read",arguments:JSON.stringify({process_session_id:processId,cursor:"0"})}}]};finish="tool_calls";
    } else if(stage>=3){delta={content:"Background result inspected."};}
    stage++;
    const send=()=>{
      res.writeHead(200,{"content-type":"text/event-stream"});
      res.write(`data: ${JSON.stringify({id:"mock",choices:[{index:0,delta,finish_reason:null}]})}\n\n`);
      res.write(`data: ${JSON.stringify({id:"mock",choices:[{index:0,delta:{},finish_reason:finish}],usage:{prompt_tokens:10,completion_tokens:10,total_tokens:20}})}\n\n`);
      res.end("data: [DONE]\n\n");
    };
    // Keep the originating Agent busy beyond process completion. Completion
    // must wait, even while no Desktop/TUI is connected.
    if(stage===2){held=send;setTimeout(()=>{held=null;send();},3000);} else send();
  });
});
await new Promise(r=>mock.listen(0,"127.0.0.1",r));
let server;
const clients=[];
function client(){
  const c=createRuntimeHostTransport({executablePath:path.join(repo,"target/debug",process.platform==="win32"?"centaeris-runtime.exe":"centaeris-runtime"),cwd:repo,environment:{...process.env,CENTAERIS_DESKTOP_DATA_DIR:profile},emitHostEvent:(...e)=>events.push(e),isAppReady:()=>true,isQuitting:()=>false,isSmokeRun:true,onRuntimeServerStarted:p=>server=p});clients.push(c);return c;
}
const call=(c,m,r)=>c.invokeCommand(m,m==="runtime/shutdown"?{}:{request:r});
const pause=ms=>new Promise(r=>setTimeout(r,ms));
async function until(f,label){const end=Date.now()+30000;while(Date.now()<end){if(await f())return;await pause(50);}throw Error(`timeout: ${label}`);}
async function outbox(){const dir=path.join(profile,"runtime/process-completions");try{return await Promise.all((await fs.readdir(dir)).filter(n=>n.endsWith(".json")).map(async n=>JSON.parse(await fs.readFile(path.join(dir,n),"utf8"))));}catch(e){if(e.code==="ENOENT")return [];throw e;}}
try{
  const c=client();await call(c,"initialize",{clientKind:"desktop",viewerId:"process-agent-desktop"});
  await call(c,"agent_runtime_config_set",{customModelProviders:[{providerId:"custom.process",name:"Process mock",baseUrl:`http://127.0.0.1:${mock.address().port}`,api:"openai-completions",models:[{model:"mock",displayName:"Mock",contextTokens:"32k",maxOutputTokens:"4k",supportsVision:false}]}]});
  await call(c,"agent_runtime_config_set",{modelProviderId:"custom.process",modelApiKey:"local-mock-only"});
  await call(c,"agent_runtime_config_set",{modelProviderId:"custom.process",model:"mock"});
  const session=await call(c,"session/new",{operationId:"process-agent-session",cwd:root,title:"Process agent test"});
  await call(c,"session/prompt",{operationId:"process-agent-prompt",sessionId:session.id,message:"Run the command in the background and inspect its result when complete."});
  await until(()=>stage>=2,"process tool result");
  const tasks=await call(c,"process_session_list",{sessionId:session.id});
  assert.equal(tasks.processes.length,1);assert.equal(tasks.processes[0].source,"agentTool");
  await c.requestAppExit();
  await pause(2100);assert.equal(stage,2,"completion must not interrupt active run");
  await until(()=>stage>=4,"automatic completion turn and output read");
  await until(async()=> (await outbox())[0]?.delivery==="delivered","durable delivery receipt");
  assert(requests[3].messages.some(m=>m.role==="tool" && m.content.includes('"text":"background-complete"')),"Agent reads plain retained output");
  const after=client();await call(after,"initialize",{clientKind:"tui",viewerId:"process-agent-reconnected"});
  await pause(700);assert.equal(stage,4,"notification must not repeat after reconnect");
  const records=await outbox();assert.equal(records.length,1);assert.equal(records[0].completion.outputComplete,true);
  // Simulate losing the outbox acknowledgement after run admission committed.
  const dir=path.join(profile,"runtime/process-completions");
  const recordName=(await fs.readdir(dir)).find(n=>n.endsWith(".json"));
  const completed=records[0];completed.delivery="pending";
  await fs.writeFile(path.join(dir,recordName),JSON.stringify(completed));
  await until(async()=>(await outbox())[0]?.delivery==="delivered","duplicate admission acknowledged");
  await pause(350);assert.equal(stage,4,"lost acknowledgement must not create another run");

  stage=0;
  const cancelled=await call(after,"session/new",{operationId:"process-agent-cancel-session",cwd:root,title:"Cancelled process origin"});
  const run=await call(after,"session/prompt",{operationId:"process-agent-cancel-prompt",sessionId:cancelled.id,message:"Start the background check."});
  await until(()=>stage===2,"cancel scenario process started");
  await call(after,"_centaeris/session/agent-runs/cancel",{sessionId:cancelled.id,agentRunId:run.agentRunId,reason:"user_cancelled"});
  await until(async()=>(await outbox()).some(r=>r.owner.sessionId===cancelled.id && r.delivery==="suppressed"),"cancelled origin suppresses continuation");
  assert.equal(stage,2,"cancellation must not be undone by completion");
  await call(after,"runtime/shutdown",{});
  await until(()=>server.exitCode!==null,"Runtime shutdown");
  completed.delivery="pending";
  await fs.writeFile(path.join(dir,recordName),JSON.stringify(completed));
  const restarted=client();await call(restarted,"initialize",{clientKind:"desktop",viewerId:"process-agent-restarted"});
  await until(async()=>(await outbox()).some(r=>r.owner.sessionId===session.id && r.delivery==="delivered"),"restart repairs lost acknowledgement");
  assert.equal(stage,2,"restart must not repeat an admitted continuation");
  assert.equal((await call(restarted,"process_session_list",{sessionId:session.id})).processes.length,0,"restart must not adopt or re-execute OS processes");
  await call(restarted,"_centaeris/session/delete",{sessionId:cancelled.id});
  assert(!(await outbox()).some(r=>r.owner.sessionId===cancelled.id),"Session deletion releases retained records");
  await call(restarted,"runtime/shutdown",{});
  console.log("PASS: model starts background process; busy run queues completion; detached follow-up reads output; reconnect/restart/lost acknowledgement deduplicate; cancelled owner is not resumed; Session deletion cleans retained results");
}finally{
  for(const c of clients)await c.requestAppExit();
  if(server?.exitCode===null)server.kill();
  mock.closeAllConnections();await new Promise(r=>mock.close(r));
  await fs.writeFile(path.join(root,"evidence.json"),JSON.stringify({stage,requests,events,held:!!held},null,2));
  console.log(`Isolated profile: ${root}`);
}
