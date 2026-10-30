// 工具执行负载：固定配置的模型请求真实bash与文件发布。
// 以恒定到达率创建Run，每次等待终态并验证实际工具与下载字节。
// Run终态、文件下载和完整验证耗时分别记录。
//
//   k6 run -e RATE=12 -e DURATION=3m scenarios/tool-execution-load.js
import { Trend, Rate } from 'k6/metrics';
import { login, configuredTarget, startRun, waitForTerminal, verifyToolRun, artifactExpectation, verifyPublishedArtifact } from './common.js';

const downloadDuration = new Trend('tool_execution_artifact_download_ms', true);
const verifiedDuration = new Trend('tool_execution_verified_duration_ms', true);
const runDuration = new Trend('tool_execution_run_duration_ms', true);
const runFailure = new Rate('tool_execution_run_failed');

export const options = {
  scenarios: {
    toolExecution: {
      executor: 'constant-arrival-rate',
      rate: Number(__ENV.RATE || 6),
      timeUnit: '1m',
      duration: __ENV.DURATION || '3m',
      preAllocatedVUs: Number(__ENV.VUS || 30),
      maxVUs: Number(__ENV.MAX_VUS || 120),
      exec: 'executeTools',
    },
  },
  thresholds: {
    'tool_execution_run_failed{scenario:toolExecution}': ['rate<0.05'],
    'tool_execution_run_duration_ms': ['p(95)<90000'],
  },
};

export function setup() {
  const expectedArtifact = artifactExpectation();
  const { workspaceId, agentId, modelId } = configuredTarget('tools');
  const { store, csrf } = login();
  return { store, csrf, workspaceId, agentId, modelId, expectedArtifact };
}

export function executeTools(data) {
  const started = Date.now();
  try {
    const { sessionId, agentRunId } = startRun(data.store, data.csrf, data.workspaceId, data.agentId, data.modelId);
    const result = waitForTerminal(data.store, data.csrf, sessionId, agentRunId, Number(__ENV.RUN_TIMEOUT || 120));
    runDuration.add(Date.now() - started);
    verifyToolRun(result, sessionId, agentRunId);
    const artifact = verifyPublishedArtifact(data.store, result, sessionId, agentRunId, data.expectedArtifact);
    downloadDuration.add(artifact.downloadMs);
    verifiedDuration.add(Date.now() - started);
    runFailure.add(0);
  } catch (error) {
    runFailure.add(1);
    console.log(`tool execution run failed to start: ${error.message}`);
  }
}
