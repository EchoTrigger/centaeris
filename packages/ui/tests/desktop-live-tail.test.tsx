import {createRef} from "react";
import {act,create,type ReactTestRenderer} from "react-test-renderer";
import {beforeEach,afterEach,expect,test,vi} from "vitest";
import type {ChatMessage} from "../src/components/chat/types";
const h=vi.hoisted(()=>({options:null as null|{count:number;estimateSize:(index:number)=>number;getItemKey:(index:number)=>string},resizes:[] as (()=>void)[],mounts:[] as string[],height:360,resized:[] as [number,number][]}));
vi.mock("@tanstack/react-virtual",()=>({useVirtualizer:(options:NonNullable<typeof h.options>)=>{
 h.options=options;
 return {getTotalSize:()=>Array.from({length:options.count},(_,i)=>options.estimateSize(i)).reduce((a,b)=>a+b,0),measurementsCache:Array.from({length:options.count},(_,i)=>({index:i,start:i*220})),getVirtualItems:()=>Array.from({length:options.count},(_,i)=>({index:i,start:i*220})),resizeItem:(index:number,size:number)=>{h.resized.push([index,size]);},measureElement:()=>{},scrollRect:{height:600}};
}}));
vi.mock("../src/components/chat/AgentResultStream",async()=>{
 const {useEffect}=await import("react");
 return {AgentResultStream:({turn}:{turn:{id:string;finalAnswer:string}})=>{useEffect(()=>{h.mounts.push(turn.id);},[turn.id]);return <p>{turn.finalAnswer}</p>;}};
});
import {VirtualMessageList} from "../src/components/chat/VirtualMessageList";
import {useChatViewStore} from "../src/components/chat/chatViewStore";
globalThis.IS_REACT_ACT_ENVIRONMENT=true;
let renderer:ReactTestRenderer;
beforeEach(()=>{useChatViewStore.getState().clear();h.height=360;h.mounts=[];h.resizes=[];h.resized=[];vi.stubGlobal("ResizeObserver",class{constructor(callback:()=>void){h.resizes.push(callback);}observe(){}disconnect(){}});});
afterEach(async()=>{await act(async()=>renderer?.unmount());vi.unstubAllGlobals();});
const message=(id:string,running:boolean,final=false):ChatMessage=>({id,role:"assistant",turn:{id,agentRunId:`run-${id}`,chunks:[],finalAnswer:`answer-${id}`,finalAnswerConfirmed:final,isStreaming:running}});
const stable=()=>{};
const props={containerRef:createRef<HTMLDivElement>(),editingUserMessageId:null,editingPrompt:"",copiedUserMessageId:null,latestUserMessageId:null,editableUserMessageId:null,onScroll:stable,onContentSizeChange:vi.fn(),onEditingPromptChange:stable,onCancelEditingUserMessage:stable,onSubmitEditedUserMessage:stable,onCopyUserMessage:stable,onStartEditingUserMessage:stable};
const mount=async(messages:ChatMessage[])=>{useChatViewStore.getState().replaceMessages(messages);await act(async()=>{renderer=create(<VirtualMessageList {...props}/>,{createNodeMock:()=>({getBoundingClientRect:()=>({height:h.height,width:800}),clientWidth:800,scrollTop:100})});});};
test("only the running last turn uses document flow, even after final starts",async()=>{
 await mount([message("old",false),message("live",true)]);
 expect(h.options?.count).toBe(1);
 const tail=renderer.root.findByProps({"data-live-tail":"live"});
 expect(tail.props.style?.position).not.toBe("absolute");
 expect(renderer.root.findAllByType("p").map(node=>node.children.join(""))).toEqual(["answer-old","answer-live"]);
 await act(async()=>useChatViewStore.getState().updateAssistantMessages([message("live",true,true) as Extract<ChatMessage,{role:"assistant"}>]));
 expect(h.options?.count).toBe(1);
});
test("completion transfers one copy to history using its measured height",async()=>{
 await mount([message("old",false),message("live",true)]);
 h.height=500;
 await act(async()=>h.resizes.forEach(callback=>callback()));
 await act(async()=>useChatViewStore.getState().updateAssistantMessages([message("live",false,true) as Extract<ChatMessage,{role:"assistant"}>]));
 expect(h.mounts.filter(id=>id==="live")).toHaveLength(1);
 expect(h.options?.count).toBe(2);
 expect(h.options?.estimateSize(1)).toBe(500);
 expect(h.resized).toContainEqual([1,500]);
 expect(renderer.root.findAll(node=>node.props["data-live-tail"]!==undefined)).toHaveLength(0);
 expect(renderer.root.findAllByType("p").map(node=>node.children.join(""))).toEqual(["answer-old","answer-live"]);
});
test("a stale running history item cannot become a resident tail",async()=>{
 await mount([message("stale",true),message("last",false)]);
 expect(h.options?.count).toBe(2);
 expect(renderer.root.findAll(node=>node.props["data-live-tail"]!==undefined)).toHaveLength(0);
});

test("prepending history keeps the same visible position and tail size changes notify the follower",async()=>{
 await mount([message("old",false),message("live",true)]);
 props.onContentSizeChange.mockClear();
 h.height=420;
 await act(async()=>h.resizes.forEach(callback=>callback()));
 expect(props.onContentSizeChange).toHaveBeenLastCalledWith(640,0,"");
 const before=props.containerRef.current!.scrollTop;
 await act(async()=>useChatViewStore.getState().replaceMessages([message("older",false),message("old",false),message("live",true)]));
 expect(props.containerRef.current!.scrollTop).toBe(before+220);
});
