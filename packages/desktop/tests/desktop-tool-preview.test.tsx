import {act, create, type ReactTestRenderer} from "react-test-renderer";
import {afterEach,beforeEach,expect,test,vi} from "vitest";
import {t} from "../src/renderer/i18n";
import {AgentResultStream} from "../src/renderer/components/chat/AgentResultStream";
import {useChatViewStore} from "../src/renderer/components/chat/chatViewStore";
import type {AssistantExecutionTurn} from "../src/renderer/components/chat/types";
import {DesktopTranscriptView} from "../src/renderer/components/chat/transcriptPaging";
const read=vi.hoisted(()=>vi.fn());
vi.mock("../src/renderer/components/chat/transcriptContentRanges",()=>({loadTranscriptContentRange:read,readCachedTranscriptContent:()=>null}));
globalThis.IS_REACT_ACT_ENVIRONMENT=true;
let renderer:ReactTestRenderer;
beforeEach(()=>{useChatViewStore.getState().clear();read.mockReset();});
afterEach(async()=>{await act(async()=>renderer?.unmount());});
const turn:AssistantExecutionTurn={id:"turn",chunks:[{id:"tool",kind:"task",task:{id:"tool",title:"read",summary:"",status:"done",provider:"tool",displayTarget:"README.md",operations:[{callId:"call",toolName:"read",status:"done"}],transcriptSessionId:"session",transcriptProjectionGeneration:"g",transcriptContentRef:{refId:"tool-output:call",revision:"1",byteLength:"90000"}}}],finalAnswer:"",isStreaming:false};
const toggle=()=>renderer.root.findByProps({className:"agent-operation-summary agent-tool-node-summary"});
test.each(["write", "edit"])("reopening a persisted %s summary without an inline diff renders and reads its output on demand",async toolName=>{
 const view=DesktopTranscriptView.open({schema:"transcript.page.v1",sessionId:"session",projectionVersion:"transcript.projection.v1",projectionGeneration:"g",sourceHighWater:"2",olderCursor:null,hasOlder:false,resumeCursors:[],blocks:[{
  blockId:"tool:mutation",blockRevision:"2",orderKey:{sourceSequence:"2",ordinal:0},
  presentation:{agentRunId:"run",sourceType:"tool_result",observedAtMs:1,displayTarget:"notes.md",durationMs:12,operation:{callId:"mutation",toolName,status:"ok",resultState:"successWithOutput",path:"notes.md",added:1,removed:0}},
  body:{kind:"tool",callId:"mutation",toolName,status:"completed",summary:"Saved notes.md",summaryRef:null,outputRef:{refId:"tool-output:mutation",revision:"2",byteLength:"14"}},
 }]});
 const message=view.materializeMessages(false)[0];
 if(message.role!=="assistant")throw new Error("Expected tool message");
 read.mockResolvedValue({content:"Saved notes.md",startOffset:"0",endOffset:"14",hasMore:false});
 await act(async()=>{renderer=create(<AgentResultStream turn={message.turn}/>);});
 expect(read).not.toHaveBeenCalled();
 expect(renderer.root.findAllByType("span").some(node=>node.children.join("").includes("notes.md"))).toBe(true);
 await act(async()=>toggle().props.onClick({preventDefault(){}}));
 expect(renderer.root.findByType("pre").children).toEqual(["Saved notes.md"]);
});
test("single tool opens bounded output directly, pages explicitly, and preserves selection after remount",async()=>{
 read.mockImplementation(async(_identity,offset)=>({content:offset==="0"?"first":"second",startOffset:offset,endOffset:offset==="0"?"65536":"90000",hasMore:offset==="0"}));
 await act(async()=>{renderer=create(<AgentResultStream turn={turn}/>);});
 expect(read).not.toHaveBeenCalled();
 expect(toggle().findAllByType("span").some(node=>node.children.join("").includes("README.md"))).toBe(true);
 await act(async()=>toggle().props.onClick({preventDefault(){}}));
 expect(read).toHaveBeenCalledTimes(1);
 expect(renderer.root.findByType("pre").children).toEqual(["first"]);
 expect(renderer.root.findByProps({role:"region"}).props["aria-label"]).toBe("Tool output");
 const next=renderer.root.findAllByType("button").find(node=>node.children.includes(t("transcriptText.nextOutput")));
 expect(next).toBeDefined();
 await act(async()=>next!.props.onClick());
 expect(read).toHaveBeenCalledTimes(2);
 expect(renderer.root.findByType("pre").children).toEqual(["second"]);
 await act(async()=>renderer.unmount());
 await act(async()=>{renderer=create(<AgentResultStream turn={turn}/>);});
 expect(toggle().props["aria-expanded"]).toBe(true);
});
test("closing a pending preview drops its late body",async()=>{
 let finish:(page:unknown)=>void=()=>{};
 read.mockImplementation(()=>new Promise(resolve=>{finish=resolve;}));
 await act(async()=>{renderer=create(<AgentResultStream turn={turn}/>);});
 await act(async()=>toggle().props.onClick({preventDefault(){}}));
 expect(renderer.root.findAllByProps({role:"status"})).toHaveLength(1);
 await act(async()=>toggle().props.onClick({preventDefault(){}}));
 await act(async()=>finish({content:"late",endOffset:"4",hasMore:false}));
 expect(renderer.root.findAllByType("pre")).toHaveLength(0);
});


test("completion retains the actual output page without issuing another read", async () => {
 read.mockImplementation(async (_identity, offset) => ({content: offset === "0" ? "first" : "second", startOffset: offset, endOffset: offset === "0" ? "65536" : "90000", hasMore: offset === "0"}));
 await act(async () => { renderer = create(<AgentResultStream turn={{...turn, isStreaming: true}} />); });
 await act(async () => toggle().props.onClick({preventDefault(){}}));
 const next = renderer.root.findAllByType("button").find(node => node.children.includes(t("transcriptText.nextOutput")))!;
 await act(async () => next.props.onClick());
 expect(renderer.root.findByType("pre").children).toEqual(["second"]);
 expect(read).toHaveBeenCalledTimes(2);
 await act(async () => renderer.update(<AgentResultStream turn={{...turn, finalAnswer: "Final answer"}} />));
 expect(renderer.root.findByType("pre").children).toEqual(["second"]);
 expect(read).toHaveBeenCalledTimes(2);
});


test("Read and Bash share a card, show actual targets and omit persisted timing", async () => {
 const tools = [
  {id:"read-card",title:"read",summary:"",status:"done" as const,provider:"tool" as const,durationMs:7000,modelContent:"file body",operations:[{callId:"read-card",toolName:"read",status:"done" as const,path:"README.md",durationMs:7000}]},
  {id:"bash-card",title:"bash",summary:"",status:"done" as const,provider:"tool" as const,durationMs:8000,modelContent:"command body",normalizedInput:{command:"git status --short"},operations:[{callId:"bash-card",toolName:"bash",status:"done" as const,durationMs:8000}]},
 ];
 await act(async () => {renderer=create(<AgentResultStream turn={{id:"cards",chunks:tools.map(task=>({id:task.id,kind:"task",task})),finalAnswer:"",isStreaming:false}}/>);});
 const cards=renderer.root.findAllByProps({className:"agent-tool-node-list"});
 const titles=cards.flatMap(card=>card.findAllByProps({className:"agent-tool-node-action is-inline-summary"}).map(node=>node.children.join("")));
 expect(titles).toEqual(["Read README.md","Ran git status --short"]);
 expect(renderer.root.findAllByType("pre")).toHaveLength(0);
 for (const button of renderer.root.findAllByProps({className:"agent-operation-summary agent-tool-node-summary"})) {
  await act(async()=>button.props.onClick({preventDefault(){}}));
 }
 expect(renderer.root.findAllByType("pre").map(node=>node.children.join(""))).toEqual(["file body","git status --short","command body"]);
 expect(read).not.toHaveBeenCalled();
});
