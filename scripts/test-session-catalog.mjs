// Isolated, real Host protocol acceptance. No external model requests.
import { createRuntimeHostTransport } from "../packages/desktop/src/runtimeHostTransport.mjs";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import assert from "node:assert/strict";
const root=await fs.mkdtemp(path.join(os.tmpdir(),"centaeris-catalog-smoke-"));
const repo=path.resolve(import.meta.dirname,"..");
let server;
const clients=[];
function client(){
  const value=createRuntimeHostTransport({executablePath:path.join(repo,"target/debug",process.platform==="win32"?"centaeris-runtime.exe":"centaeris-runtime"),cwd:repo,environment:{...process.env,CENTAERIS_DESKTOP_DATA_DIR:path.join(root,"profile"),CENTAERIS_PROVIDER_POLLING_HOST_ENABLED:"false",CENTAERIS_RUNTIME_GC_HOST_ENABLED:"false",CENTAERIS_SUBAGENT_SCHEDULER_HOST_ENABLED:"false"},emitHostEvent:()=>{},isAppReady:()=>false,isQuitting:()=>false,isSmokeRun:true,onRuntimeServerStarted:p=>{server=p;}});
  clients.push(value);return value;
}
const call=(client,method,request)=>client.invokeCommand(method,{request});
try{
  const desktop=client(),tui=client();
  await call(desktop,"initialize",{clientKind:"desktop",viewerId:"catalog-desktop"});
  await call(tui,"initialize",{clientKind:"tui",viewerId:"catalog-tui"});
  const watch=await call(desktop,"session/catalog",{mode:"changes"});
  const sessions=[];
  for(let i=0;i<6;i++) sessions.push(await call(tui,"session/new",{operationId:`catalog-create-${i}`,title:`Chat ${i}`,cwd:root}));
  const cwd=sessions[0].cwd;
  assert.ok(cwd);
  const first=await call(desktop,"session/catalog",{mode:"recent",cwd,limit:2});
  assert.equal(first.items.length,2);assert.ok(first.nextCursor);
  const next=await call(desktop,"session/catalog",{mode:"recent",cwd,limit:2,cursor:first.nextCursor});
  assert.equal(new Set([...first.items,...next.items].map(i=>i.id)).size,4);
  await call(tui,"_centaeris/session/update_metadata",{sessionId:sessions[0].id,title:"Changed",isPinned:true});
  const stale=await call(desktop,"session/catalog",{mode:"recent",cwd,limit:2,cursor:first.nextCursor});
  assert.equal(stale.reset,true);
  const pinned=await call(desktop,"session/catalog",{mode:"pinned"});
  assert.equal(pinned.items[0].id,sessions[0].id);
  const removed=await call(tui,"_centaeris/session/delete",{sessionId:sessions[1].id});
  assert.deepEqual(removed.deletedSessionIds,[sessions[1].id]);
  let cursor=watch.revision;const seen=new Set();const deleted=new Set();
  for(let i=0;i<10;i++){
    const changes=await call(desktop,"session/catalog",{mode:"changes",cursor,limit:2});
    assert.equal(changes.reset,false);assert.ok(changes.items.length+changes.deletedIds.length<=2);
    changes.items.forEach(item=>seen.add(item.id));changes.deletedIds.forEach(id=>deleted.add(id));cursor=changes.revision;
    if(!changes.nextCursor)break;
  }
  assert.equal(seen.size,5);assert.ok(deleted.has(sessions[1].id));
  const unchanged=await call(tui,"session/catalog",{mode:"changes",cursor});
  assert.deepEqual(unchanged.items,[]);assert.deepEqual(unchanged.deletedIds,[]);
  await assert.rejects(call(desktop,"session/catalog",{mode:"recent",banana:true}));
  console.log("PASS: bounded Host pages, cross-client changes, pin/delete, stale cursors, strict requests");
}finally{
  for(const client of clients)await client.requestAppExit();
  if(server?.exitCode===null)server.kill();
  console.log(`Isolated profile: ${root}`);
}
