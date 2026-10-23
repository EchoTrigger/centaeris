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
 const scroll=createMessageScroll({measure:()=>geometry,setPadding:()=>{},scrollTo:value=>{geometry.scrollTop=value;},reducedMotion:()=>true,onFollowingChange:()=>{}});
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


test.each(["wheel down", "PageDown", "End", "Space", "touch down", "scrollbar"])(
  "%s takes control before deferred content growth can move the reader", (gesture) => {
    const geometry = { height: 400, contentEnd: 1000, anchorTop: 0, scrollTop: 600 };
    const scroll = createMessageScroll({ measure: () => geometry, setPadding: () => {},
      scrollTo: top => { geometry.scrollTop = top; }, reducedMotion: () => true, onFollowingChange: () => {} });
    const noop = () => {};
    act(() => { renderer = create(<VirtualMessageList containerRef={createRef()} onReadingIntent={() => scroll.pause()}
      onContentSizeChange={() => scroll.update()} onScroll={() => scroll.userScroll()}
      editingUserMessageId={null} editingPrompt="" copiedUserMessageId={null} latestUserMessageId={null} editableUserMessageId={null}
      onEditingPromptChange={noop} onCancelEditingUserMessage={noop} onSubmitEditedUserMessage={noop}
      onCopyUserMessage={noop} onStartEditingUserMessage={noop} />); });
    const viewport = renderer.root.findByProps({ className: "messages-container uiRsMessagesContainer" });
    // More content has landed, before the scheduled follower frame.
    geometry.contentEnd += 300;
    const currentTarget = { scrollTop: geometry.scrollTop, scrollHeight: geometry.contentEnd, clientHeight: geometry.height };
    act(() => {
      if (gesture === "wheel down") viewport.props.onWheel({ deltaY: 100, currentTarget });
      else if (gesture === "touch down") {
        viewport.props.onTouchStart({ touches: [{ clientY: 300 }] });
        viewport.props.onTouchMove({ touches: [{ clientY: 250 }], currentTarget });
      } else if (gesture === "scrollbar") {
        const target = {};
        viewport.props.onPointerDown?.({ target, currentTarget: target });
      } else viewport.props.onKeyDown({ key: gesture === "Space" ? " " : gesture, shiftKey: false, currentTarget,
        target: { closest: () => null } });
    });
    scroll.update();
    expect(geometry.scrollTop).toBe(600);
  },
);


test("scrolling downward at the real end leaves live following enabled", () => {
  const geometry = { height: 400, contentEnd: 1000, anchorTop: 0, scrollTop: 600 };
  const scroll = createMessageScroll({ measure: () => geometry, setPadding: () => {},
    scrollTo: top => { geometry.scrollTop = top; }, reducedMotion: () => true, onFollowingChange: () => {} });
  const noop = () => {};
  act(() => { renderer = create(<VirtualMessageList containerRef={createRef()} onReadingIntent={() => scroll.pause()}
    onContentSizeChange={() => scroll.update()} onScroll={() => scroll.userScroll()}
    editingUserMessageId={null} editingPrompt="" copiedUserMessageId={null} latestUserMessageId={null} editableUserMessageId={null}
    onEditingPromptChange={noop} onCancelEditingUserMessage={noop} onSubmitEditedUserMessage={noop}
    onCopyUserMessage={noop} onStartEditingUserMessage={noop} />); });
  const viewport = renderer.root.findByProps({ className: "messages-container uiRsMessagesContainer" });
  act(() => viewport.props.onWheel({ deltaY: 100, currentTarget: { scrollTop: 600, scrollHeight: 1000, clientHeight: 400 } }));
  geometry.contentEnd += 300;
  scroll.update();
  expect(geometry.scrollTop).toBe(900);
});
