import {createRef} from "react";
import {act,create,type ReactTestRenderer} from "react-test-renderer";
import {afterEach,expect,test,vi} from "vitest";
import {VirtualMessageList} from "../src/components/chat/VirtualMessageList";
import {createMessageScroll} from "../src/components/chat/messageScroll";
const harness=vi.hoisted(()=>({virtualizer:{
 getTotalSize:()=>1000,getVirtualItems:()=>[],measurementsCache:[],scrollOffset:600,scrollDirection:null,
 shouldAdjustScrollPositionOnItemSizeChange:undefined as undefined|((item:{start:number;end:number},delta:number,instance:unknown)=>boolean),
}}));
vi.mock("@tanstack/react-virtual",()=>({useVirtualizer:()=>harness.virtualizer}));
globalThis.IS_REACT_ACT_ENVIRONMENT=true;
let renderer:ReactTestRenderer;
afterEach(()=>act(()=>renderer?.unmount()));

test("manual disclosure grows below its title without following the new bottom",()=>{
 const geometry={height:400,contentEnd:1000,anchorTop:0,scrollTop:600};
 const scroll=createMessageScroll({measure:()=>geometry,setPadding:()=>{},scrollTo:value=>{geometry.scrollTop=value;},requestFrame:()=>1,cancelFrame:()=>{},reducedMotion:()=>true,onFollowingChange:()=>{}});
 const noop=()=>{};
 act(()=>{renderer=create(<VirtualMessageList containerRef={createRef()} onReadingIntent={()=>scroll.pause()}
  onContentSizeChange={()=>scroll.update()} onScroll={()=>scroll.userScroll()} editingUserMessageId={null} editingPrompt="" copiedUserMessageId={null} latestUserMessageId={null} editableUserMessageId={null}
  onEditingPromptChange={noop} onCancelEditingUserMessage={noop} onSubmitEditedUserMessage={noop} onCopyUserMessage={noop} onStartEditingUserMessage={noop}/>);});
 const container=renderer.root.findByProps({className:"messages-container uiRsMessagesContainer"});
 // Click capture also covers a keyboard-activated disclosure button.
 act(()=>container.props.onClickCapture?.({target:{closest:()=>({})}}));
 geometry.contentEnd+=240;scroll.update();
 expect(geometry.scrollTop).toBe(600);
 // Delayed content and continuing generation must preserve the reading position.
 geometry.contentEnd+=120;scroll.update();expect(geometry.scrollTop).toBe(600);
 scroll.jump();expect(geometry.scrollTop).toBe(960);
 const resize=(start:number,end:number)=>{
  const adjust=harness.virtualizer.shouldAdjustScrollPositionOnItemSizeChange;
  if(adjust?adjust({start,end},240,harness.virtualizer):start<harness.virtualizer.scrollOffset) geometry.scrollTop+=240;
 };
 geometry.scrollTop=600;resize(100,1000);expect(geometry.scrollTop).toBe(600);
 resize(100,300);expect(geometry.scrollTop).toBe(840);
});
