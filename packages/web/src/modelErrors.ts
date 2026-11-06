type Translate = (key: string) => string;

export const MODEL_ERROR_KEYS: Readonly<Record<string, string>> = {
  provider_authentication_failed: "modelErrors.authenticationFailed",
  provider_secret_unavailable: "modelErrors.secretUnavailable",
  provider_secret_decryption_failed: "modelErrors.secretUnavailable",
  provider_unavailable: "modelErrors.serviceUnavailable",
  model_unavailable: "modelErrors.modelUnavailable",
  provider_rate_limited: "modelErrors.rateLimited",
  provider_busy_or_rate_limited: "modelErrors.rateLimited",
  provider_request_rejected: "modelErrors.requestRejected",
  model_thinking_mode_unsupported: "modelErrors.thinkingUnsupported",
  provider_timeout: "modelErrors.timeout",
  timeout: "modelErrors.timeout",
  auth_failed: "modelErrors.authenticationFailed",
  provider_unreachable: "modelErrors.unreachable",
  network: "modelErrors.unreachable",
  provider_stream_interrupted: "modelErrors.interrupted",
  provider_response_interrupted: "modelErrors.interrupted",
  provider_interrupted: "modelErrors.interrupted",
  provider_response_invalid: "modelErrors.interrupted",
  empty_final_response: "modelErrors.interrupted",
  model_adapter_failed: "modelErrors.failed",
  completion_tool_delivery_required: "modelErrors.replyNotDelivered",
  completion_delivery_repair_prompt_too_large: "modelErrors.replyContextLimit",
  execution_input_rejected: "modelErrors.executionInputRejected",
  execution_start_unknown: "modelErrors.executionOutcomeUnknown",
  execution_input_unknown: "modelErrors.executionOutcomeUnknown",
  execution_query_unknown: "modelErrors.executionOutcomeUnknown",
  execution_output_unavailable: "modelErrors.executionOutputUnavailable",
  stopped: "modelErrors.executionInterrupted",
  shutdown: "modelErrors.executionInterrupted",
  "The model response was interrupted. Retry the request.": "modelErrors.interrupted",
  "AgentRun did not complete. Retry the request.": "modelErrors.failed",
  "The execution environment was interrupted. Retry the request.": "modelErrors.executionInterrupted",
  "The task was started, but this input was not accepted because execution resources are exhausted. Do not repeat the command.": "modelErrors.executionInputRejected",
  "The task start outcome is unknown. Do not repeat the operation.": "modelErrors.executionOutcomeUnknown",
  "The task was started, but input delivery is unconfirmed. Do not repeat the operation.": "modelErrors.executionOutcomeUnknown",
  "Task execution result could not be confirmed. Do not repeat the operation.": "modelErrors.executionOutcomeUnknown",
  "Task output is unavailable. The command must not be repeated automatically.": "modelErrors.executionOutputUnavailable",
};

function modelErrorKey(reason: string | null | undefined) {
  return reason && Object.hasOwn(MODEL_ERROR_KEYS, reason) ? MODEL_ERROR_KEYS[reason] : null;
}

export function localizedModelError(reason: string, t: Translate): string | null {
  const key = modelErrorKey(reason);
  return key ? t(key) : /^(?:model|provider|prepared_prompt)_[a-z0-9_]+$/.test(reason) ? t("modelErrors.failed") : null;
}

export function modelFailureDetail(message: string | null, t: Translate, reasonType?: string | null): string {
  return t(modelErrorKey(reasonType) ?? modelErrorKey(message) ?? "modelErrors.failed");
}

export function modelErrorText(error: unknown, t: Translate): string {
  const payload = typeof error === "object" && error !== null && "payload" in error ? error.payload : null;
  const reason = typeof payload === "object" && payload !== null && "reasonType" in payload
    && typeof payload.reasonType === "string" ? payload.reasonType : null;
  const message = error instanceof Error ? error.message : String(error || "model_adapter_failed");
  return (reason ? localizedModelError(reason, t) : null) ?? localizedModelError(message, t) ?? t("modelErrors.failed");
}
