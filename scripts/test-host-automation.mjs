import { DatabaseSync } from "node:sqlite";
// Local transport acceptance: mock model and an OpenSSH argv fixture, no remote host.
import {createRuntimeHostTransport} from '../packages/desktop/src/runtimeHostTransport.mjs';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
const repo=process.cwd();
const root=await fs.mkdtemp(path.join(os.tmpdir(),'centaeris-automation-'));
const profile=path.join(root,'profile');
const bin=path.join(root,'bin');await fs.mkdir(bin);
await fs.writeFile(path.join(root,'ssh.rs'),'fn main(){for a in std::env::args().skip(1){println!("{}",a);} }');
execFileSync('rustc',[path.join(root,'ssh.rs'),'-o',path.join(bin,process.platform==='win32'?'ssh.exe':'ssh')]);
const requests=[];
let hold=false;
const mock=http.createServer((req,res)=>{
 let body='';req.setEncoding('utf8');req.on('data',c=>body+=c);req.on('end',()=>{
  requests.push(JSON.parse(body));
  const send=()=>{res.writeHead(200,{'content-type':'text/event-stream'});res.write(`data: ${JSON.stringify({id:'mock',choices:[{index:0,delta:{content:'Scheduled inspection complete.'},finish_reason:null}]})}\n\n`);res.write(`data: ${JSON.stringify({id:'mock',choices:[{index:0,delta:{},finish_reason:'stop'}],usage:{prompt_tokens:10,completion_tokens:5,total_tokens:15}})}\n\n`);res.end('data: [DONE]\n\n');};
  if(hold)setTimeout(send,2000);else send();
 });
});
await new Promise(r=>mock.listen(0,'127.0.0.1',r));
let server;const clients=[];
function client(){const c=createRuntimeHostTransport({executablePath:path.join(repo,'target/debug',process.platform==='win32'?'centaeris-runtime.exe':'centaeris-runtime'),cwd:repo,environment:{...process.env,PATH:bin+path.delimiter+process.env.PATH,CENTAERIS_DESKTOP_DATA_DIR:profile},emitHostEvent:()=>{},isAppReady:()=>true,isQuitting:()=>false,isSmokeRun:true,onRuntimeServerStarted:p=>server=p});clients.push(c);return c;}
const call=(c,m,r)=>c.invokeCommand(m,m==='runtime/shutdown'?{}:{request:r});
const manage=(c,r)=>call(c,'schedule_manage',r);
const pause=ms=>new Promise(r=>setTimeout(r,ms));
async function until(f,label){const end=Date.now()+30000;while(Date.now()<end){if(await f())return;await pause(50);}throw Error(`timeout: ${label}`);}
const initialize=c=>call(c,'initialize',{clientKind:'tui',viewerId:`automation-${clients.length}`});
const history=(c,id)=>manage(c,{action:'history',scheduleId:id});
try{
 const c=client();await initialize(c);
 const other=client();await initialize(other);
 await call(c,'agent_runtime_config_set',{customModelProviders:[{providerId:'custom.scheduler',name:'Scheduler mock',baseUrl:`http://127.0.0.1:${mock.address().port}`,api:'openai-completions',models:['mock','other'].map(model=>({model,displayName:model,contextTokens:'32k',maxOutputTokens:'4k',supportsVision:false}))}]});
 await call(c,'agent_runtime_config_set',{modelProviderId:'custom.scheduler',modelApiKey:'local-mock-only'});
 await call(c,'agent_runtime_config_set',{modelProviderId:'custom.scheduler',model:'mock'});
 const spec={name:'inspection',cwd:root,prompt:'Inspect this local fixture.',cron:'0 9 * * *',at:null,timezone:'Asia/Taipei',model:{providerId:'custom.scheduler',model:'mock',thinkingMode:null}};
 const creation={action:'create',operationId:'create-inspection',spec};
 const cli=path.join(repo,'target/debug',process.platform==='win32'?'centa.exe':'centa');
 const cliOptions={cwd:root,encoding:'utf8',env:{...process.env,PATH:bin+path.delimiter+process.env.PATH,CENTAERIS_DESKTOP_DATA_DIR:profile,CENTAERIS_RUNTIME_EXE:path.join(repo,'target/debug',process.platform==='win32'?'centaeris-runtime.exe':'centaeris-runtime')}};
 assert(execFileSync(cli,['ssh','connect','fixture-host'],cliOptions).includes('fixture-host'));
 const [plan,replay]=await Promise.all([manage(c,creation),manage(other,creation)]);assert.equal(plan.id,replay.id);
 const cliList=JSON.parse(execFileSync(cli,['schedule','list'],cliOptions));assert.equal(cliList.schedules[0].id,plan.id);
 const specFile=path.join(root,'schedule spec.json');await fs.writeFile(specFile,JSON.stringify(spec));
 assert.equal(JSON.parse(execFileSync(cli,['schedule','create',specFile,'create-inspection'],cliOptions)).id,plan.id);
 await assert.rejects(manage(c,{...creation,spec:{...spec,prompt:'changed'}}),/conflict/);
 await assert.rejects(manage(c,{action:'list',unknown:true}),/unknown|invalid/i);
 await assert.rejects(manage(c,{action:'update',scheduleId:plan.id,expectedRevision:99,spec}),/revision conflict/);
 await call(c,'agent_runtime_config_set',{modelProviderId:'custom.scheduler',model:'other'});
 // Paused plans can run manually; the scheduler remains independent of clients.
 await manage(c,{action:'pause',scheduleId:plan.id});hold=true;
 const manual={action:'run',scheduleId:plan.id,operationId:'manual-one'};
 const run=await manage(c,manual);assert.equal((await manage(other,manual)).id,run.id);
 await until(()=>requests.length===1,'model receives scheduled occurrence');
 assert.equal(requests[0].model,'mock','plan selection must not mutate/follow global model');
 await assert.rejects(manage(c,{...manual,operationId:'manual-two'}),/still active/);
 await manage(c,{action:'delete',scheduleId:plan.id});
 await until(async()=>(await history(c,plan.id)).runs[0]?.status==='succeeded','delete does not cancel admitted run');
 assert.equal((await call(c,'agent_runtime_config_get',{})).model,'other');
 hold=false;
 // Future automatic plans keep the Runtime alive after both clients disconnect.
 const future=await manage(c,{action:'create',operationId:'future',spec});
 await manage(c,{action:'service',enabled:true});
 await c.requestAppExit();await other.requestAppExit();await pause(6500);assert.equal(server.exitCode,null,'enabled scheduler survives UI detach and idle timeout');
 const after=client();await initialize(after);
 const one=await manage(after,{action:'create',operationId:'once',spec:{...spec,cron:null,at:Date.now()+1500}});
 await until(async()=>(await history(after,one.id)).runs[0]?.status==='succeeded','one-shot trigger');
 assert.equal(requests.length,2);
 const sshSession=await call(after,'session/new',{operationId:'ssh-fixture-session',cwd:root,title:'SSH fixture'});
 const list=await call(after,'process_session_list',{sessionId:sshSession.id});
 const sshRequest={sessionId:sshSession.id,serviceInstanceId:list.serviceInstanceId,operationId:'ssh-once',destination:'fixture-host',command:"printf '$HOME'; git status",timeoutMs:10000};
 const ssh=await call(after,'ssh_start',sshRequest);assert.equal((await call(after,'ssh_start',sshRequest)).processSessionId,ssh.processSessionId);
 let page;await until(async()=>{page=await call(after,'process_session_read',{sessionId:sshSession.id,processSessionId:ssh.processSessionId,cursor:'0',waitMs:0});return page.process.outputComplete;},'SSH fixture output');
 const output=page.chunks.map(c=>Buffer.from(c.dataBase64,'base64').toString()).join('');
 assert.equal(page.process.exitCode,0,output);assert(output.includes('BatchMode=yes'));assert(!output.includes('ConnectTimeout='));assert(output.includes(sshRequest.command));
 await assert.rejects(call(after,'ssh_start',{...sshRequest,operationId:'invalid',destination:'-oProxyCommand=bad'}),/destination/);
 // Save a lost acknowledgement while offline, then prove deterministic recovery.
 await call(after,'runtime/shutdown',{});await until(()=>server.exitCode!==null,'shutdown');
 const db=new DatabaseSync(path.join(profile,'runtime/schedules.sqlite3'));
 const completed=JSON.parse(db.prepare("SELECT body FROM records WHERE namespace='runs' AND owner=?").get(one.id).body);
 completed.status='pending';completed.sessionId=null;
 db.prepare("UPDATE records SET body=?,state='active' WHERE namespace='runs' AND id=?").run(JSON.stringify(completed),completed.id);
 // Restart coalesces missed recurring times into one latest occurrence.
 const fp=JSON.parse(db.prepare("SELECT body FROM records WHERE namespace='plans' AND id=?").get(future.id).body);
 fp.spec.cron='* * * * *';fp.nextAt=Date.now()-600000;
 db.prepare("UPDATE records SET body=?,due=? WHERE namespace='plans' AND id=?").run(JSON.stringify(fp),fp.nextAt,fp.id);
 db.close();
 const restarted=client();await initialize(restarted);
 await until(async()=>(await history(restarted,one.id)).runs[0]?.status==='succeeded','lost acknowledgement recovery');
 await until(async()=>(await history(restarted,future.id)).runs.some(r=>r.status==='succeeded'),'coalesced catch-up');
 assert.equal(requests.length,3,'restart recovers admitted work and executes exactly one catch-up');
 assert((await history(restarted,one.id)).runs[0].sessionId,'recovered history links to Session');
 await manage(restarted,{action:'service',enabled:false});
 await call(restarted,'runtime/shutdown',{});
 console.log('PASS: two-client deduplication, pinned model, manual/one-shot execution, overlap, delete isolation, background keepalive, SSH argv/output/replay, lost-ack restart and bounded misfires');
} finally {
 for(const c of clients)await c.requestAppExit();
 if(server?.exitCode===null)server.kill();mock.closeAllConnections();await new Promise(r=>mock.close(r));
 console.log(`Isolated acceptance profile: ${profile}`);
}
