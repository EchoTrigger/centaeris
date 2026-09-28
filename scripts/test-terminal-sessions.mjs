// Run after cargo build -p centaeris-runtime --bin centaeris-runtime.
// Uses an isolated profile and two real clients; never sends model requests.
import { createRuntimeHostTransport } from "../packages/desktop/src/runtimeHostTransport.mjs";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";
const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const root = await fs.mkdtemp(
  path.join(os.tmpdir(), "centaeris-terminal-smoke-"),
);
const binary = path.join(
  repo,
  "target/debug",
  process.platform === "win32" ? "centaeris-runtime.exe" : "centaeris-runtime",
);
const environment = {
  ...process.env,
  CENTAERIS_DESKTOP_DATA_DIR: path.join(root, "profile"),
  CENTAERIS_PROVIDER_POLLING_HOST_ENABLED: "false",
  CENTAERIS_RUNTIME_GC_HOST_ENABLED: "false",
  CENTAERIS_SUBAGENT_SCHEDULER_HOST_ENABLED: "false",
  CENTAERIS_RUNTIME_GARBAGE_MAINTENANCE_ENABLED: "false",
};
let server;
const connections = [];
function client() {
  const c = createRuntimeHostTransport({
    executablePath: binary,
    cwd: repo,
    environment,
    emitHostEvent: () => {},
    isAppReady: () => false,
    isQuitting: () => false,
    isSmokeRun: true,
    onRuntimeServerStarted: (p) => {
      server = p;
    },
  });
  connections.push(c);
  return c;
}
const call = (c, method, request) =>
  c.invokeCommand(method, method === "runtime/shutdown" ? {} : { request });
const pause = (ms) => new Promise((r) => setTimeout(r, ms));

const terminal=(c,request)=>call(c,"terminal_manage",request);
try {
 const a=client();let b=client();
 await call(a,"initialize",{clientKind:"desktop",viewerId:"terminal-a"});
 await call(b,"initialize",{clientKind:"desktop",viewerId:"terminal-b"});
 const list=await terminal(a,{action:"list",workspaceRoot:root});
 const request={operationId:"terminal-first",serviceInstanceId:list.serviceInstanceId,workspaceRoot:root,sessionId:null,shell:null,cols:80,rows:24};
 const first=await terminal(a,{action:"start",request});
 assert.equal((await terminal(b,{action:"start",request})).terminalId,first.terminalId);
 const target={serviceInstanceId:list.serviceInstanceId,terminalId:first.terminalId};
 await assert.rejects(terminal(a,{action:"resize",target,cols:0,rows:24}));
 await terminal(a,{action:"resize",target,cols:110,rows:35});
 const send=async(command)=>terminal(a,{action:"write",target,dataBase64:Buffer.from(command).toString("base64")});
 // ConPTY requests the initial cursor position; emulate a terminal response.
 if(process.platform === "win32") {
  for(let i=0;i<50;i++){const page=await terminal(b,{action:"read",target,cursor:"0"});if(page.chunks.some(c=>Buffer.from(c.dataBase64,"base64").includes(Buffer.from("\x1b[6n")))){await send("\x1b[1;1R");break;}await pause(50);}
 }
 await pause(600);
 const marker=path.join(root,"interactive.txt");
 const command=process.platform==="win32"?`[IO.File]::WriteAllText('${marker.replaceAll("'","''")}', 'terminal-ready')\r`:`printf terminal-ready > '${marker}'\r`;
 await send(command.replaceAll("\\r","\r"));
 const end=Date.now()+12000;while(Date.now()<end){if(await fs.readFile(marker,"utf8").catch(()=>"")==="terminal-ready")break;await pause(100);}
 assert.equal(await fs.readFile(marker,"utf8"),"terminal-ready");

 const interrupted=path.join(root,"interrupt-ready");
 await send(process.platform==="win32"?"Start-Sleep 30\r":"sleep 30\r");await pause(400);await send("\x03");await pause(300);
 await send(process.platform==="win32"?`[IO.File]::WriteAllText('${interrupted}', 'ready')\r`:`touch '${interrupted}'\r`);
 for(let i=0;i<60;i++){if(await fs.access(interrupted).then(()=>true,()=>false))break;await pause(50);}
 await fs.access(interrupted);
 await assert.rejects(terminal(b,{action:"read",target:{...target,serviceInstanceId:"old-service"},cursor:"0"}));
 await assert.rejects(terminal(b,{action:"start",request:{...request,cols:90}}));
 const page=await terminal(b,{action:"read",target,cursor:"0"});assert.ok(page.chunks.length);
 const same=await terminal(a,{action:"read",target,cursor:"0"});assert.ok(same.chunks.length);
 await a.requestAppExit();await b.requestAppExit();await pause(6500);
 b=client();await call(b,"initialize",{clientKind:"desktop",viewerId:"terminal-reconnected"});
 assert.equal((await terminal(b,{action:"read",target,cursor:page.nextCursor})).terminal.state,"running");

 const childMarker=path.join(root,"child-alive");
 const startedMarker=path.join(root,"child-started");
 const childScript=`[IO.File]::WriteAllText('${startedMarker}', 'started'); Start-Sleep 4; [IO.File]::WriteAllText('${childMarker}', 'alive')`;
 const encoded=Buffer.from(childScript,"utf16le").toString("base64");
 const child=process.platform==="win32"?`Start-Process -FilePath '${first.shell}' -WindowStyle Hidden -ArgumentList '-NoProfile','-EncodedCommand','${encoded}'\r`:`(touch '${startedMarker}'; sleep 4; touch '${childMarker}') &\r`;
 await terminal(b,{action:"write",target,dataBase64:Buffer.from(child.replaceAll("\\r","\r")).toString("base64")});
 const childEnd=Date.now()+6000;
 while(Date.now()<childEnd){if(await fs.access(startedMarker).then(()=>true,()=>false))break;await pause(50);}
 await fs.access(startedMarker);
 await terminal(b,{action:"stop",target});
 let stopped;for(let i=0;i<100;i++){stopped=await terminal(b,{action:"read",target,cursor:"0"});if(stopped.terminal.state==="exited")break;await pause(50);}
 assert.equal(stopped.terminal.state,"exited");
 await pause(4000);await assert.rejects(fs.access(childMarker));
 const second=await terminal(b,{action:"start",request:{...request,operationId:"terminal-second"}});
 assert.notEqual(second.terminalId,first.terminalId);
 await call(b,"runtime/shutdown",{});await pause(2000);assert.notEqual(server.exitCode,null);
 console.log("PASS terminal PTY input/output, resize, create deduplication, independent readers, detached survival, stop tree and service shutdown");
} finally {for(const c of connections)await c.requestAppExit();if(server?.exitCode===null)server.kill();console.log(`Isolated profile: ${root}`);}
