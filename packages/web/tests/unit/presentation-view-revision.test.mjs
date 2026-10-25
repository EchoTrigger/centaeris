import assert from 'node:assert/strict';
import test from 'node:test';
import { WorkspacePresentationController } from '../../src/chat/workspacePresentationController.ts';
import { validatePresentationFrame } from '../../src/chat/workspacePresentationProtocol.ts';
import { createTranscriptViewStore } from '../../src/chat/transcriptViewStore.ts';
const identity = {workspaceId:'workspace',agentId:'agent',sessionId:null};
const frame = (contextRevision='0',historyRevision='1',workRevision='2') => ({schema:'workspace.presentation.v1',kind:'viewRevision',...identity,contextRevision,historyRevision,workRevision});
test('view revisions publish through the same scoped presentation controller and coalesce replays', async()=>{
  const received=[];
  const controller=new WorkspacePresentationController({...identity,store:createTranscriptViewStore(),onViewRevision:value=>received.push(value)});
  for(const value of [frame(),frame(),frame('0','0','1'),frame('0','3','2')]) await controller.acceptWithBackpressure({cursor:null,item:value});
  assert.equal(received.length,2);
  assert.equal(received.at(-1).historyRevision,'3');
});
test('view revision wire fields are exact bounded decimal versions',()=>{
  assert.equal(validatePresentationFrame(frame(),identity).workRevision,'2');
  for(const value of [ {...frame(),historyRevision:'01'}, {...frame(),workRevision:2}, {...frame(),contextRevision:'18446744073709551616'}, {...frame(),extra:true}]) assert.throws(()=>validatePresentationFrame(value,identity));
});
