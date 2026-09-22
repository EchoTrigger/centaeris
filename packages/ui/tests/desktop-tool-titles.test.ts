import { expect, test } from "vitest";
import { collectTimelineOperations, formatToolGroupTitle, formatOperationInlineSummary } from "../src/components/chat/toolActivityTranscriptModel";
import type { TimelineOperation } from "../src/components/chat/types";

test("single read and bash summaries use actual targets before generic prose", () => {
 const read = {taskId:"t",taskTitle:"tool",callId:"r",toolName:"read",status:"done",displayTarget:"src/main.rs",text:"read"} as TimelineOperation;
 expect(formatToolGroupTitle([read])).toBe("Read src/main.rs");
 expect(formatOperationInlineSummary(read,"done")).toBe("src/main.rs");
 const bash = {taskId:"t",taskTitle:"tool",callId:"b",toolName:"bash",status:"done",normalizedInput:{command:"cargo check --locked",description:"Check project"}} as TimelineOperation;
 expect(formatToolGroupTitle([bash])).toContain("cargo check --locked");
 expect(formatOperationInlineSummary(bash,"done")).toContain("cargo check --locked");
 const search = {taskId:"t",taskTitle:"tool",callId:"s",toolName:"read",status:"done",query:"TODO"} as TimelineOperation;
 expect(formatToolGroupTitle([search])).toContain("TODO");
});

test("group summary and detail retain every tool beyond thirty-two entries",()=>{
 const tasks=Array.from({length:40},(_,index)=>({id:`t${index}`,title:"read",summary:"",status:"done" as const,provider:"tool" as const,operations:[{callId:`c${index}`,toolName:"read",status:"done",path:`file${index}`}]}));
 const operations=collectTimelineOperations(tasks);
 expect(operations).toHaveLength(40);
 expect(formatToolGroupTitle(operations)).toBe("Read 40 files");
});
