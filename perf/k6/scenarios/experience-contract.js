export function experienceTask(kind, sampleId, workspace = {}) {
  if (!/^[a-f0-9]{32}$/.test(sampleId)) throw new Error('synthetic experience identity required');
  const marker = 'PERF_EXPERIENCE_' + sampleId;
  let commands, finalText;
  if (kind === 'text') { commands = []; finalText = marker + '_TEXT_OK'; }
  else if (kind === 'single-tool') {
    finalText = marker + '_TOOL_OK';
    commands = [{ command: `printf '${finalText}\\n'`, output: finalText }];
  } else if (kind === 'multi-step') {
    const path = '/mnt/data/perf-experience-' + sampleId + '.txt';
    finalText = marker + '_MULTI_OK:42';
    commands = [
      { command: `printf '21\\n' > '${path}' && printf '${marker}_CREATED\\n'`, output: marker + '_CREATED' },
      { command: `python3 -c 'from pathlib import Path; p=Path("${path}"); print("${marker}_MULTI_OK:"+str(int(p.read_text())*2)); p.unlink()'`, output: finalText },
    ];
  } else if (kind === 'workspace') {
    if (!/^[a-f0-9]{32}$/.test(workspace.fixtureId) || !['first', 'followup'].includes(workspace.turn)) {
      throw new Error('explicit workspace fixture and turn required');
    }
    const path = '/mnt/data/perf-latency-' + workspace.fixtureId + '.bin';
    finalText = marker + '_TOOL_OK';
    const action = workspace.turn === 'first'
      ? `dd if=/dev/urandom of=${path} bs=1048576 count=4 status=none && sha256sum ${path} >${path}.sha256`
      : `test "$(wc -c <${path})" -eq 4194304 && sha256sum -c ${path}.sha256`;
    commands = [{ command: action + ` && printf '${finalText}\\n'`, output: finalText }];
  } else throw new Error('unsupported experience task');
  const prompt = commands.length
    ? `Synthetic performance task. Invoke bash exactly ${commands.length} time(s), sequentially, with the following exact command(s). `
      + 'Do not combine commands, add other tool calls, or publish artifacts. Use timeout_ms=10000 for each bash call. '
      + commands.map((value, index) => `Command ${index + 1}:\n${value.command}`).join('\n')
      + `\nAfter checking the tool outputs, reply with exactly ${finalText}`
    : `Synthetic performance task. Do not call any tools. Reply with exactly ${finalText}`;
  return { prompt, commands, finalText };
}

export function verifyExperienceRun(result, contract, sessionId, agentRunId) {
  if (result?.status !== 'completed' || !Array.isArray(result.events)) throw new Error('experience Run did not complete');
  const calls = new Map(), eventIds = new Set();
  let sequence = 0, completed = false, nextCall = 0, nextResult = 0, finalText = null;
  for (const event of result.events) {
    if (!event || event.sessionId !== sessionId || event.agentRunId !== agentRunId
        || !Number.isInteger(event.sequence) || event.sequence <= sequence || completed
        || typeof event.eventId !== 'string' || !event.eventId || eventIds.has(event.eventId)) throw new Error('experience event binding invalid');
    sequence = event.sequence; eventIds.add(event.eventId);
    const payload = event.payload;
    if (event.type === 'tool_call') {
      const expected = contract.commands[nextCall];
      if (!expected || nextCall !== nextResult || typeof payload?.callId !== 'string' || !payload.callId
          || calls.has(payload.callId) || payload.toolName !== 'bash' || payload.normalizedInput?.command !== expected.command) {
        throw new Error('experience command contract failed');
      }
      calls.set(payload.callId, nextCall++);
    } else if (event.type === 'tool_result') {
      if (calls.get(payload?.callId) !== nextResult || payload?.toolName !== 'bash'
          || !['successWithOutput', 'successNoOutput'].includes(payload.resultState)
          || typeof payload.modelContent !== 'string' || !payload.modelContent.includes(contract.commands[nextResult]?.output)) {
        throw new Error('experience tool output contract failed');
      }
      nextResult++;
    } else if (event.type === 'assistant_message') {
      if (payload?.status === 'done') finalText = payload.modelMarkdown;
    } else if (event.type === 'artifact_published') throw new Error('unexpected experience publication');
    else if (['agent_run_completed', 'agent_run_failed', 'agent_run_interrupted'].includes(event.type)) {
      if (event.type !== 'agent_run_completed') throw new Error('experience terminal failed');
      completed = true;
    }
  }
  if (!completed || nextCall !== contract.commands.length || nextResult !== nextCall
      || typeof finalText !== 'string' || finalText.trim() !== contract.finalText) throw new Error('experience result contract failed');
  return { toolCount: nextResult };
}

// Called only after the complete command/output/order contract passes. Project
// explicit identity/numeric/enum fields so model commands and output cannot leak.
export function verifiedToolTimings(result, expectedCount) {
  if (!Array.isArray(result.toolTimings) || result.toolTimings.length !== expectedCount) throw new Error('per-tool observations missing');
  const calls = result.events.filter(event => event.type === 'tool_call' && event.payload.toolName === 'bash');
  const eventTime = value => Number.isSafeInteger(value) && value >= 0 ? value : null;
  return result.toolTimings.map((timing, index) => {
    const call = calls[index];
    const completion = result.events.find(event => event.type === 'tool_result' && event.payload.callId === call?.payload.callId);
    const callCreated = eventTime(call?.createdAtMs), resultCreated = eventTime(completion?.createdAtMs);
    const serverState = callCreated !== null && resultCreated !== null && resultCreated >= callCreated ? 'observed' : 'notObserved';
    const callObserved = timing?.toolCallObservedAtMs, resultObserved = timing?.toolResultObservedAtMs;
    if (!Number.isSafeInteger(callObserved) || callObserved < 0 || !Number.isSafeInteger(resultObserved) || resultObserved < 0) throw new Error('client callback times missing');
    const clientState = resultObserved >= callObserved ? 'observed' : 'notObserved';
    const clientInterval = clientState === 'observed' ? resultObserved - callObserved : null;
    if (!timing || typeof timing.callId !== 'string' || !timing.callId || timing.callId.length > 160
        || timing.callId !== call?.payload.callId || timing.toolIndex !== index + 1 || timing.toolName !== 'bash'
        || timing.resultState !== completion?.payload.resultState
        || timing.toolCallCreatedAtMs !== callCreated || timing.toolResultCreatedAtMs !== resultCreated
        || timing.serverTimingState !== serverState || timing.clientTimingState !== clientState
        || timing.clientCallToResultMs !== clientInterval) throw new Error('per-tool timing binding invalid');
    return { callId: timing.callId, toolIndex: timing.toolIndex, toolName: 'bash',
      toolCallCreatedAtMs: callCreated, toolResultCreatedAtMs: resultCreated,
      toolCallObservedAtMs: callObserved, toolResultObservedAtMs: resultObserved,
      clientCallToResultMs: clientInterval, resultState: timing.resultState,
      serverTimingState: serverState, clientTimingState: clientState, contractState: 'verified' };
  });
}

