// Agent-first scheduling acceptance: natural-language prompt -> tool context -> create -> enable.
import {createRuntimeHostTransport} from '../packages/desktop/src/runtimeHostTransport.mjs';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import assert from 'node:assert/strict';
const repo=process.cwd(), root=await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(),'centaeris-agent-schedule-')));
const errors=[];let stage=0, finished=false, context, planId, server;
const mock=http.createServer((req,res)=>{
 let body='';req.setEncoding('utf8');req.on('data',c=>body+=c);req.on('end',()=>{
  let delta,finish='tool_calls';
  try{
   const payload=JSON.parse(body);
   const last=()=>JSON.parse(payload.messages.filter(m=>m.role==='tool').at(-1).content);
   const tool=args=>({tool_calls:[{index:0,id:`schedule-call-${stage}`,type:'function',function:{name:'schedule_manage',arguments:JSON.stringify(args)}}]});
   if(stage===0)delta=tool({action:'context'});
   else if(stage===1){context=last();assert.equal(context.cwd,root);assert.equal(context.model.model,'mock');assert.equal(context.serviceEnabled,false);assert(context.now);delta=tool({action:'create',operation_id:'agent-created-plan',spec:{name:'daily-review',cwd:context.cwd,prompt:'Review local changes.',cron:'0 9 * * *',at:null,timezone:'UTC',model:{provider_id:context.model.providerId,model:context.model.model,thinking_mode:context.model.thinkingMode}}});}
   else if(stage===2){const plan=last();assert(plan.id);planId=plan.id;delta=tool({action:'service',enabled:true});}
   else {assert.equal(last().serviceEnabled,true);delta={content:'Created the daily 09:00 UTC review and enabled local scheduling.'};finish='stop';finished=true;}
  }catch(e){errors.push(e);delta={content:'Scheduling failed.'};finish='stop';finished=true;}
  stage++;res.writeHead(200,{'content-type':'text/event-stream'});res.write(`data: ${JSON.stringify({id:'mock',choices:[{index:0,delta,finish_reason:null}]})}\n\n`);res.write(`data: ${JSON.stringify({id:'mock',choices:[{index:0,delta:{},finish_reason:finish}],usage:{prompt_tokens:10,completion_tokens:10,total_tokens:20}})}\n\n`);res.end('data: [DONE]\n\n');
 });
});
await new Promise(r=>mock.listen(0,'127.0.0.1',r));
const c=createRuntimeHostTransport({executablePath:path.join(repo,'target/debug',process.platform==='win32'?'centaeris-runtime.exe':'centaeris-runtime'),cwd:repo,environment:{...process.env,CENTAERIS_DESKTOP_DATA_DIR:path.join(root,'profile')},emitHostEvent:()=>{},isAppReady:()=>true,isQuitting:()=>false,isSmokeRun:true,onRuntimeServerStarted:p=>server=p});
const call=(m,r)=>c.invokeCommand(m,m==='runtime/shutdown'?{}:{request:r});
try{
 await call('initialize',{clientKind:'desktop',viewerId:'agent-schedule-test'});
 await call('agent_runtime_config_set',{customModelProviders:[{providerId:'custom.scheduler',name:'Scheduler mock',baseUrl:`http://127.0.0.1:${mock.address().port}`,api:'openai-completions',models:[{model:'mock',contextTokens:'32k',maxOutputTokens:'4k',supportsVision:false}]}]});
 await call('agent_runtime_config_set',{modelProviderId:'custom.scheduler',modelApiKey:'local-mock-only'});
 await call('agent_runtime_config_set',{modelProviderId:'custom.scheduler',model:'mock'});
 const session=await call('session/new',{operationId:'agent-schedule-session',cwd:root,title:'Create schedule naturally'});
 await call('session/prompt',{operationId:'agent-schedule-request',sessionId:session.id,message:'Schedule a review of this workspace every day at 09:00 UTC, using the current model. Enable local scheduling. Create it for me; do not ask me to write JSON or run a command.'});
 const end=Date.now()+30000;while(!finished&&Date.now()<end)await new Promise(r=>setTimeout(r,50));
 assert(finished,'Agent did not complete scheduling');if(errors.length)throw errors[0];
 const result=await call('schedule_manage',{action:'list'});assert.equal(result.serviceEnabled,true);assert.equal(result.schedules.length,1);assert.equal(result.schedules[0].id,planId);assert.deepEqual(result.schedules[0].spec.model,context.model);
 assert(!JSON.stringify(context).includes('local-mock-only'),'tool context must not disclose credentials');
 await call('runtime/shutdown',{});
 console.log('PASS: Agent reads scheduling context, creates a durable plan with explicit model/effort, enables the service and confirms success without user CLI/JSON');
}finally{await c.requestAppExit();if(server?.exitCode===null)server.kill();mock.closeAllConnections();await new Promise(r=>mock.close(r));console.log(`Isolated profile: ${root}`);}
