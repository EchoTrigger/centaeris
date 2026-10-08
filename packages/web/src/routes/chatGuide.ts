import type { DelegationGuide } from "./appDelegationGuide";

export function chatGuide(base: string, agentId: string): DelegationGuide {
  const create = { agentId, businessUserId: "customer-42" };
  const submit = { text: "Process this user's request.", fileIds: [] };
  const data = { id: "returned-message-id", chatId: "returned-chat-id", author: "user",
    ...submit, createdAt: "2026-01-01T00:00:00.000Z", readAt: null };
  const path = "/api/v1/chats";
  const steps: DelegationGuide["steps"] = [
    { key: "createChat", method: "POST", path: base + path, requiredScopes: ["assistant:use"],
      request: JSON.stringify(create, null, 2), response: JSON.stringify({ data: {
        id: "returned-chat-id", ...create, createdAt: data.createdAt } }, null, 2) },
    { key: "submitMessage", method: "POST", path: base + path + "/returned-chat-id/messages",
      requiredScopes: ["messages:submit"], headers: { "Idempotency-Key": "business-input-001" },
      request: JSON.stringify(submit, null, 2), response: JSON.stringify({ data }, null, 2) },
    { key: "readMessages", method: "GET", path: base + path + "/returned-chat-id/messages",
      requiredScopes: ["sessions:read"], request: null, response: JSON.stringify({ data: [data], nextCursor: null, hasMore: false }, null, 2) },
    { key: "readEvents", method: "GET", path: base + path + "/returned-chat-id/events",
      requiredScopes: ["sessions:read"], request: null,
      response: "id: <opaque cursor>\nevent: message.created\ndata: <Message JSON>\n\n" },
  ];
  const javascript = `// Backend only. Persist the request key and exact body before sending.
const API_BASE = ${JSON.stringify(base)};
const token = process.env.CENTAERIS_TOKEN;
if (!token) throw new Error("Set the backend credential");
async function request(path, body, key) {
  const headers = { Authorization: "Bearer " + token };
  if (body !== undefined) headers["Content-Type"] = "application/json";
  if (key) headers["Idempotency-Key"] = key;
  const response = await fetch(API_BASE + path, {
    method: body === undefined ? "GET" : "POST", headers,
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  if (!response.ok) throw new Error("HTTP " + response.status);
  return response.json();
}
const { data: chat } = await request("${path}", ${JSON.stringify(create)});
const messagesPath = "${path}/" + encodeURIComponent(chat.id) + "/messages";
const accepted = await request(messagesPath, ${JSON.stringify(submit)}, "business-input-001");
const history = await request(messagesPath);
console.log(accepted, history);
// History is a snapshot. Continue older pages with ?cursor=nextCursor while hasMore.
// Subscribe to /events for message.created and message.updated; resume with Last-Event-ID.
// Upsert by message id. Saved or read messages do not imply task completion.`;
  const python = `# Backend only. Keep the request key and original body for retries.
import json, os
from urllib.request import Request, urlopen
from urllib.parse import quote
API_BASE = ${JSON.stringify(base)}
TOKEN = os.environ["CENTAERIS_TOKEN"]
def request(path, body=None, key=None):
    headers = {"Authorization": "Bearer " + TOKEN}
    if key:
        headers["Idempotency-Key"] = key
    data = None
    if body is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(body).encode("utf-8")
    with urlopen(Request(API_BASE + path, data=data, headers=headers), timeout=30) as response:
        return json.load(response)
chat = request("${path}", ${JSON.stringify(create)})["data"]
messages_path = "${path}/" + quote(chat["id"], safe="") + "/messages"
accepted = request(messages_path, ${JSON.stringify(submit)}, "business-input-001")
history = request(messages_path)
print(accepted, history)
# Continue older pages using nextCursor. Subscribe to /events and persist each SSE id.
# Apply message.created and message.updated by message id; reconnect with Last-Event-ID.`;
  const shell = (value: string) => "'" + value.replaceAll("'", "'\"'\"'") + "'";
  const curl = `# Backend POSIX shell; requires curl and jq. Keep the retry key and original body.
set -eu
API_BASE=${shell(base)}
TOKEN="$CENTAERIS_TOKEN"
test -n "$TOKEN"
chat=$(curl --fail-with-body --silent --show-error "$API_BASE${path}" \\
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \\
  --data ${shell(JSON.stringify(create))})
CHAT_ID=$(printf '%s' "$chat" | jq -er '.data.id | @uri')
curl --fail-with-body --silent --show-error "$API_BASE${path}/$CHAT_ID/messages" \\
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \\
  -H "Idempotency-Key: business-input-001" --data ${shell(JSON.stringify(submit))}
curl --fail-with-body --silent --show-error "$API_BASE${path}/$CHAT_ID/messages" \\
  -H "Authorization: Bearer $TOKEN"
# Live feed (persist SSE ids and send Last-Event-ID when reconnecting):
curl --fail-with-body --silent --show-error --no-buffer "$API_BASE${path}/$CHAT_ID/events" \\
  -H "Authorization: Bearer $TOKEN" -H "Accept: text/event-stream"`;
  return { kind: "native", steps, examples: { javascript, python, curl } };
}
