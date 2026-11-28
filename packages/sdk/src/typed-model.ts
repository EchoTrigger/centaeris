import type { EvaluateTypedRequest, JsonValue, ReadTypedOperationRequest, TypedModelReceipt, TypedModelRequest, TypedOperation } from "./types.ts";
import { RpcProtocolError } from "./index.ts";

function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function exact(value: Record<string, unknown>, required: string[], optional: string[] = []): boolean {
  return required.every((key) => Object.hasOwn(value, key))
    && Object.keys(value).every((key) => required.includes(key) || optional.includes(key));
}
function json(value: unknown): value is JsonValue {
  if (value === null || typeof value === "string" || typeof value === "boolean") return true;
  if (typeof value === "number") return Number.isFinite(value);
  if (Array.isArray(value)) return value.every(json);
  return record(value) && Object.values(value).every(json);
}
function probability(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 && value <= 1;
}

function sameJson(left: unknown, right: unknown): boolean {
  if (left === right) return true;
  if (Array.isArray(left) && Array.isArray(right)) {
    return left.length === right.length && left.every((item, index) => sameJson(item, right[index]));
  }
  return record(left) && record(right) && Object.keys(left).length === Object.keys(right).length
    && Object.entries(left).every(([key, item]) => Object.hasOwn(right, key) && sameJson(item, right[key]));
}
/** Decode a projection of CET's durable native typed-model attempt. */
export function decodeTypedModelReceipt(value: unknown, expected: EvaluateTypedRequest): TypedModelReceipt {
  const bad = () => { throw new RpcProtocolError("Runtime returned an invalid typed-model receipt"); };
  if (!record(value) || !exact(value, [
    "operationId", "sessionId", "attemptId", "requestId", "upstreamRequestId", "responseId",
    "recordPath", "request", "response", "usage",
  ])) return bad();
  if (value.operationId !== expected.operationId || value.sessionId !== expected.sessionId
    || ![value.attemptId, value.requestId, value.recordPath].every((item) => typeof item === "string" && item.length > 0)
    || ![value.upstreamRequestId, value.responseId].every((item) => item === null || typeof item === "string")
    || !json(value.usage)) return bad();
  const request = value.request;
  const response = value.response;
  if (!record(request) || !exact(request, ["model", "state", "questions"])
    || typeof request.model !== "string" || !request.model
    || (expected.model !== undefined && request.model !== expected.model)
    || !json(request.state) || !record(request.questions)
    || !record(response) || !exact(response, ["model", "answers"], ["id", "usage"])
    || response.model !== request.model || !record(response.answers)
    || (response.id !== undefined && typeof response.id !== "string")
    || (response.usage !== undefined && !json(response.usage))) return bad();
  if (!sameJson(request.state, expected.state) || !sameJson(request.questions, expected.questions)
    || !sameJson(value.usage, response.usage ?? null)) return bad();
  const questions = request.questions;
  const answers = response.answers;
  if (Object.keys(questions).length === 0 || Object.keys(answers).length !== Object.keys(questions).length
    || Object.keys(questions).length !== Object.keys(expected.questions).length) return bad();
  for (const [id, question] of Object.entries(questions)) {
    if (!Object.hasOwn(expected.questions, id) || !record(question)
      || !exact(question, ["type", "instructions", "criteria"])
      || !(typeof question.instructions === "string" || record(question.instructions))
      || !json(question.instructions) || !record(question.criteria)
      || Object.keys(question.criteria).length === 0
      || !Object.values(question.criteria).every((description) => typeof description === "string" && description.trim().length > 0)) return bad();
    const criteria = question.criteria;
    const answer = answers[id];
    if (!record(answer)) return bad();
    if (question.type === "choice") {
      if (!exact(answer, ["type", "choice"], ["confidence", "probabilities"])
        || answer.type !== "choice" || typeof answer.choice !== "string" || !Object.hasOwn(question.criteria, answer.choice)
        || (answer.confidence !== undefined && !probability(answer.confidence))) return bad();
      if (answer.probabilities !== undefined && (!record(answer.probabilities)
        || Object.keys(answer.probabilities).length !== Object.keys(question.criteria).length
        || Object.entries(answer.probabilities).some(([key, p]) => !Object.hasOwn(criteria, key) || !probability(p)))) return bad();
    } else if (question.type === "noul") {
      if (!exact(question.criteria, ["true", "false"]) || !exact(answer, ["type", "noul"])
        || answer.type !== "noul" || !probability(answer.noul)) return bad();
    } else return bad();
  }
  if (value.responseId !== (response.id ?? null)) return bad();
  return value as unknown as TypedModelReceipt;
}

function nativeRequest(value: unknown): value is TypedModelRequest {
  if (!record(value) || !exact(value, ["model", "state", "questions"])
    || typeof value.model !== "string" || !value.model.trim() || !json(value.state)
    || !record(value.questions) || Object.keys(value.questions).length === 0) return false;
  return Object.entries(value.questions).every(([id, question]) => {
    if (!id.trim() || !record(question) || !exact(question, ["type", "instructions", "criteria"])
      || !(record(question.instructions) || (typeof question.instructions === "string" && question.instructions.trim()))
      || !json(question.instructions) || !record(question.criteria) || Object.keys(question.criteria).length === 0
      || Object.entries(question.criteria).some(([key, description]) => !key.trim() || typeof description !== "string" || !description.trim())) return false;
    return question.type === "choice" || (question.type === "noul" && exact(question.criteria, ["true", "false"]));
  });
}

/** Read original CET evidence without interpreting an invalid native answer. */
export function decodeTypedOperation(value: unknown, expected: ReadTypedOperationRequest): TypedOperation | null {
  if (value === null) return null;
  const bad = () => { throw new RpcProtocolError("Runtime returned an invalid typed-operation record"); };
  if (!record(value) || !exact(value, [
    "operationId", "sessionId", "attemptId", "requestId", "recordPath", "request", "rawResponse", "failure", "receipt",
  ]) || value.operationId !== expected.operationId || value.sessionId !== expected.sessionId
    || ![value.attemptId, value.requestId, value.recordPath].every((item) => typeof item === "string" && item.length > 0)
    || !nativeRequest(value.request) || !json(value.failure)) return bad();
  const raw = value.rawResponse;
  if (raw !== null && (!record(raw) || !exact(raw, ["statusCode", "headers", "body"])
    || typeof raw.statusCode !== "number" || !Number.isInteger(raw.statusCode) || raw.statusCode < 100 || raw.statusCode > 599
    || !record(raw.headers) || !Object.values(raw.headers).every((header) => typeof header === "string")
    || typeof raw.body !== "string")) return bad();
  if (value.receipt !== null) {
    const receipt = decodeTypedModelReceipt(value.receipt, { ...expected, ...value.request });
    if (receipt.attemptId !== value.attemptId || receipt.requestId !== value.requestId || receipt.recordPath !== value.recordPath) return bad();
  }
  return value as unknown as TypedOperation;
}
