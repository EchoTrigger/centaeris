type Translate = (key: string) => string;

const MODEL_ERROR_KEYS: Readonly<Record<string, string>> = {
  model_quota_domain_required: "modelErrors.quotaRequired",
  model_quota_domain_disabled: "modelErrors.quotaDisabled",
  model_quota_backend_unavailable: "modelErrors.quotaUnavailable",
  provider_authentication_failed: "modelErrors.authenticationFailed",
  provider_secret_unavailable: "modelErrors.secretUnavailable",
  provider_secret_decryption_failed: "modelErrors.secretUnavailable",
  provider_unavailable: "modelErrors.modelUnavailable",
  provider_rate_limited: "modelErrors.rateLimited",
  provider_request_rejected: "modelErrors.requestRejected",
  model_thinking_mode_unsupported: "modelErrors.thinkingUnsupported",
  provider_timeout: "modelErrors.timeout",
  provider_unreachable: "modelErrors.unreachable",
  provider_stream_interrupted: "modelErrors.interrupted",
  model_adapter_failed: "modelErrors.failed",
  completion_tool_delivery_required: "modelErrors.replyNotDelivered",
  completion_delivery_repair_prompt_too_large: "modelErrors.replyContextLimit",
  "The model response was interrupted. Retry the request.": "modelErrors.interrupted",
  "AgentRun did not complete. Retry the request.": "modelErrors.failed",
  "The execution environment was interrupted. Retry the request.": "modelErrors.executionInterrupted",
};

export function localizedModelError(reason: string, t: Translate): string | null {
  const key = Object.hasOwn(MODEL_ERROR_KEYS, reason) ? MODEL_ERROR_KEYS[reason] : null;
  return key ? t(key) : /^(?:model|provider|prepared_prompt)_[a-z0-9_]+$/.test(reason) ? t("modelErrors.failed") : null;
}

export function modelFailureDetail(reason: string, t: Translate): string {
  if (/^(?:model|provider|prepared_prompt|completion)_[a-z0-9_]+$/.test(reason)) {
    const key = Object.hasOwn(MODEL_ERROR_KEYS, reason) ? MODEL_ERROR_KEYS[reason] : null;
    const label = key && key !== "modelErrors.failed" ? t(key) : t("modelErrors.failureLabel");
    return `${label} · ${reason}`;
  }
  return localizedModelError(reason, t) ?? reason;
}

export function modelErrorText(error: unknown, t: Translate): string {
  const payload = typeof error === "object" && error !== null && "payload" in error ? error.payload : null;
  const reason = typeof payload === "object" && payload !== null && "reasonType" in payload
    && typeof payload.reasonType === "string" ? payload.reasonType : null;
  const message = error instanceof Error ? error.message : String(error || "model_adapter_failed");
  return (reason ? localizedModelError(reason, t) : null) ?? localizedModelError(message, t) ?? message;
}
