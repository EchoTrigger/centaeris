type DelegationTarget = { agentId: string | null; definitionId: string | null; workspaceId: string };
type GuideStep = {
  key: string; method: "GET" | "POST"; path: string; requiredScopes: string[];
  request: string | null; response: string;
};
export type DelegationGuide = {
  kind: "native" | "published"; steps: GuideStep[];
  examples: { curl: string; python: string; javascript: string };
};

const pretty = (value: unknown) => JSON.stringify(value, null, 2);
const shell = (value: string) => "'" + value.replaceAll("'", "'\"'\"'") + "'";
const sampleInputData = { inputId: "business-input-001", body: "Process this user's request." };
const sampleInput = { schema: "agent.input.submit.v1", ...sampleInputData, attachmentRefs: [] };
const receipt = (command: string, operationId: string) => ({
  operationId, command, status: "accepted", sessionId: "returned-session-id",
  agentRunId: command === "createSession" ? null : "returned-run-id",
  turnId: command === "createSession" ? null : "returned-turn-id",
});

/** Display-only examples. Credentials are read by the caller's backend. */
export function buildDelegationGuide(apiBaseUrl: string, target: DelegationTarget): DelegationGuide {
  const url = new URL(apiBaseUrl);
  if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) {
    throw new Error("Invalid configured API address");
  }
  const base = apiBaseUrl.replace(/\/+$/, "");
  if (!target.workspaceId || Boolean(target.agentId) === Boolean(target.definitionId)) {
    throw new Error("Expected exactly one delegation target and a Workspace");
  }
  const step = (key: string, method: "GET" | "POST", path: string, scope: string, request: unknown, response: unknown): GuideStep => ({
    key, method, path: base + path, requiredScopes: [scope],
    request: request === null ? null : pretty(request),
    response: typeof response === "string" ? response : pretty(response),
  });
  if (target.agentId) {
    const resolvePath = "/api/agents/" + encodeURIComponent(target.agentId) + "/business-branches/resolve";
    return {
      kind: "native",
      steps: [
        step("resolveBranch", "POST", resolvePath, "assistant:use", { businessUserId: "customer-42" }, {
          schema: "agent.business_branch.v1", branchId: "returned-branch-id", rootAgentId: target.agentId,
          businessUserId: "customer-42", agentId: "returned-agent-id", sessionId: "returned-session-id",
        }),
        step("submitInput", "POST", "/api/agents/returned-agent-id/inputs", "messages:submit", sampleInput, {
          schema: "agent.input.accepted.v1", agentId: "returned-agent-id", sessionId: "returned-session-id",
          input: { ...sampleInputData, attachments: [], sequence: 1, createdAtMs: 0, read: null },
        }),
        step("readInputs", "GET", "/api/agents/returned-agent-id/inputs?afterSequence=0&limit=50", "sessions:read", null, {
          schema: "agent.inputs.v1", agentId: "returned-agent-id", sessionId: "returned-session-id",
          inputs: [{ ...sampleInputData, attachments: [], sequence: 1, createdAtMs: 0, read: null }], nextAfterSequence: null,
        }),
        step("readMessages", "GET", "/api/agents/returned-agent-id/messages?afterSequence=0&limit=50", "sessions:read", null, {
          schema: "agent.messages.v1", agentId: "returned-agent-id", sessionId: "returned-session-id",
          messages: [], nextAfterSequence: null,
        }),
      ],
      examples: examples(base, { kind: "native", resolvePath }),
    };
  }
  const workspacePath = "/api/workspaces/" + encodeURIComponent(target.workspaceId);
  const instancePath = workspacePath + "/available-agent-definitions/" + encodeURIComponent(target.definitionId!) + "/instance";
  const createdAt = "2026-01-01T00:00:00Z";
  return {
    kind: "published",
    steps: [
      step("obtainInstance", "POST", instancePath, "assistant:use", {}, {
        agent: { id: "returned-agent-id", workspaceId: target.workspaceId, definitionId: target.definitionId,
          definitionVersionId: "published-version-id", name: "Published assistant", description: "",
          instructions: "Assistant instructions", avatarKind: "centaeris", status: "active",
          deletedAt: null, createdAt, updatedAt: createdAt },
      }),
      step("selectModel", "GET", "/api/models", "messages:submit", null, {
        models: [{ id: "returned-model-id", displayName: "Enabled model", providerId: null, providerDisplayName: null,
          modelName: "configured-model", contextTokens: 32000, maxOutputTokens: 4096, thinkingMode: null, thinkingModes: [] }],
      }),
      step("createSession", "POST", workspacePath + "/sessions", "sessions:create",
        { operationId: "create-session-001", agentId: "returned-agent-id" }, receipt("createSession", "create-session-001")),
      step("submitMessage", "POST", workspacePath + "/sessions/returned-session-id/messages", "messages:submit",
        { operationId: "submit-message-001", text: "Hello", modelConfigRef: "returned-model-id" }, receipt("submitMessage", "submit-message-001")),
      step("readEvents", "GET", "/api/sessions/returned-session-id/agent-runs/returned-run-id/events", "events:read", null,
        "id: <opaque SSE cursor>\ndata: <session.stream.item.v1 JSON>\n\n"),
    ],
    examples: examples(base, { kind: "published", workspacePath, instancePath }),
  };
}

type Flow = { kind: "native"; resolvePath: string } | { kind: "published"; workspacePath: string; instancePath: string };
function examples(base: string, flow: Flow): DelegationGuide["examples"] {
  const js = [
    "// Backend Node.js ES module (.mjs). Persist request identity and original body for retries.",
    "const API_BASE = " + JSON.stringify(base) + ";",
    "const token = process.env.CENTAERIS_TOKEN;",
    'if (!token) throw new Error("Set the backend credential");',
    "async function request(path, body, branchId) {",
    '  const headers = { Authorization: "Bearer " + token };',
    '  if (branchId) headers["X-Centaeris-Business-Branch-Id"] = branchId;',
    '  if (body !== undefined) headers["Content-Type"] = "application/json";',
    "  const response = await fetch(API_BASE + path, {",
    '    method: body === undefined ? "GET" : "POST", headers,',
    "    ...(body === undefined ? {} : { body: JSON.stringify(body) }),",
    "  });",
    '  if (!response.ok) throw new Error("HTTP " + response.status);',
    "  return response.json();",
    "}",
  ];
  // No branch header is present in published examples.
  if (flow.kind === "published") js.splice(6, 1);
  const py = [
    "# Backend Python; standard library. Persist request identity and original body.",
    "import json, os",
    "from urllib.request import Request, urlopen",
    "from urllib.parse import quote",
    "API_BASE = " + JSON.stringify(base),
    'TOKEN = os.environ["CENTAERIS_TOKEN"]',
    "def request(path, body=None, branch_id=None):",
    '    headers = {"Authorization": "Bearer " + TOKEN}',
    "    if branch_id:",
    '        headers["X-Centaeris-Business-Branch-Id"] = branch_id',
    "    data = None",
    "    if body is not None:",
    '        headers["Content-Type"] = "application/json"',
    '        data = json.dumps(body).encode("utf-8")',
    "    with urlopen(Request(API_BASE + path, data=data, headers=headers), timeout=30) as response:",
    "        return json.load(response)",
  ];
  if (flow.kind === "published") py.splice(8, 2);
  const sh = [
    "# Backend POSIX shell; requires curl and jq. Persist request identity and original body.",
    "set -eu",
    "API_BASE=" + shell(base),
    'TOKEN="$CENTAERIS_TOKEN"',
    'test -n "$TOKEN"',
  ];
  const curl = (path: string, body?: string, branch = false) => [
    'curl --fail-with-body --silent --show-error "$API_BASE' + path + '"',
    '  -H "Authorization: Bearer $TOKEN"',
    ...(branch ? ['  -H "X-Centaeris-Business-Branch-Id: $BRANCH_ID"'] : []),
    ...(body ? ['  -H "Content-Type: application/json" --data ' + body] : []),
  ].join(" \\\n");
  if (flow.kind === "native") {
    const input = JSON.stringify(sampleInput);
    js.push(
      'const branch = await request(' + JSON.stringify(flow.resolvePath) + ', { businessUserId: "customer-42" });',
      'const agentPath = "/api/agents/" + encodeURIComponent(branch.agentId);',
      'const accepted = await request(agentPath + "/inputs", ' + input + ", branch.branchId);",
      'const inputs = await request(agentPath + "/inputs?afterSequence=0&limit=50", undefined, branch.branchId);',
      'const messages = await request(agentPath + "/messages?afterSequence=0&limit=50", undefined, branch.branchId);',
      "console.log(accepted, inputs, messages);",
      "// First pages only: retain cursors, deduplicate messages, and recheck pending Read.",
    );
    py.push(
      "branch = request(" + JSON.stringify(flow.resolvePath) + ', {"businessUserId": "customer-42"})',
      'agent_path = "/api/agents/" + quote(branch["agentId"], safe="")',
      'accepted = request(agent_path + "/inputs", ' + input + ', branch["branchId"])',
      'inputs = request(agent_path + "/inputs?afterSequence=0&limit=50", branch_id=branch["branchId"])',
      'messages = request(agent_path + "/messages?afterSequence=0&limit=50", branch_id=branch["branchId"])',
      "print(accepted, inputs, messages)",
      "# First pages only: retain cursors, deduplicate messages, and recheck pending Read.",
    );
    sh.push(
      "branch=$(" + curl(flow.resolvePath, shell('{"businessUserId":"customer-42"}')) + ")",
      'AGENT_ID=$(printf \'%s\' "$branch" | jq -er \'.agentId | @uri\')',
      'BRANCH_ID=$(printf \'%s\' "$branch" | jq -er \'.branchId\')',
      curl("/api/agents/$AGENT_ID/inputs", shell(input), true),
      curl("/api/agents/$AGENT_ID/inputs?afterSequence=0&limit=50", undefined, true),
      curl("/api/agents/$AGENT_ID/messages?afterSequence=0&limit=50", undefined, true),
      "# First pages only: retain cursors, deduplicate messages, and recheck pending Read.",
    );
  } else {
    js.push(
      "const instance = await request(" + JSON.stringify(flow.instancePath) + ", {});",
      'const { models } = await request("/api/models");',
      'if (!models.length) throw new Error("No usable model");',
      "const created = await request(" + JSON.stringify(flow.workspacePath + "/sessions") + ', { operationId: "create-session-001", agentId: instance.agent.id });',
      "const sessionId = encodeURIComponent(created.sessionId);",
      "const accepted = await request(" + JSON.stringify(flow.workspacePath + "/sessions/") + ' + sessionId + "/messages",',
      '  { operationId: "submit-message-001", text: "Hello", modelConfigRef: models[0].id });',
      'const eventsPath = "/api/sessions/" + sessionId + "/agent-runs/" + encodeURIComponent(accepted.agentRunId) + "/events";',
      'const stream = await fetch(API_BASE + eventsPath, { headers: { Authorization: "Bearer " + token, Accept: "text/event-stream" } });',
      'if (!stream.ok || !stream.body) throw new Error("HTTP " + stream.status);',
      "const reader = stream.body.getReader();",
      "const decoder = new TextDecoder();",
      "while (true) {",
      "  const { done, value } = await reader.read();",
      "  if (done) break;",
      "  console.log(decoder.decode(value, { stream: true }));",
      "}",
      "// Choose the intended models[].id; reconnect using that Run's original SSE id.",
    );
    py.push(
      "instance = request(" + JSON.stringify(flow.instancePath) + ", {})",
      'models = request("/api/models")["models"]',
      "if not models:",
      '    raise RuntimeError("No usable model")',
      "created = request(" + JSON.stringify(flow.workspacePath + "/sessions") + ', {"operationId": "create-session-001", "agentId": instance["agent"]["id"]})',
      'session_id = quote(created["sessionId"], safe="")',
      "accepted = request(" + JSON.stringify(flow.workspacePath + "/sessions/") + ' + session_id + "/messages",',
      '    {"operationId": "submit-message-001", "text": "Hello", "modelConfigRef": models[0]["id"]})',
      'events_path = "/api/sessions/" + session_id + "/agent-runs/" + quote(accepted["agentRunId"], safe="") + "/events"',
      'headers = {"Authorization": "Bearer " + TOKEN, "Accept": "text/event-stream"}',
      "with urlopen(Request(API_BASE + events_path, headers=headers), timeout=30) as response:",
      "    for line in response:",
      '        print(line.decode("utf-8"), end="")',
      "# Choose the intended models[].id; reconnect using that Run's original SSE id.",
    );
    sh.push(
      "instance=$(" + curl(flow.instancePath, "'{}'") + ")",
      'AGENT_ID=$(printf \'%s\' "$instance" | jq -er \'.agent.id\')',
      "models=$(" + curl("/api/models") + ")",
      'MODEL_ID=$(printf \'%s\' "$models" | jq -er \'.models[0].id\')',
      'create_body=$(jq -n --arg agent "$AGENT_ID" \'{operationId:"create-session-001",agentId:$agent}\')',
      "created=$(" + curl(flow.workspacePath + "/sessions", '"$create_body"') + ")",
      'SESSION_ID=$(printf \'%s\' "$created" | jq -er \'.sessionId | @uri\')',
      'message_body=$(jq -n --arg model "$MODEL_ID" \'{operationId:"submit-message-001",text:"Hello",modelConfigRef:$model}\')',
      "accepted=$(" + curl(flow.workspacePath + "/sessions/$SESSION_ID/messages", '"$message_body"') + ")",
      'RUN_ID=$(printf \'%s\' "$accepted" | jq -er \'.agentRunId | @uri\')',
      curl("/api/sessions/$SESSION_ID/agent-runs/$RUN_ID/events") + ' \\\n  --no-buffer -H "Accept: text/event-stream"',
      "# Choose the intended models[].id; reconnect using that Run's original SSE id.",
    );
  }
  return { curl: sh.join("\n"), python: py.join("\n"), javascript: js.join("\n") };
}
