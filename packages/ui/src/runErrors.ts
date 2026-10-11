import catalog from "../../../assets/run-error-messages.json";

const messages: Readonly<Record<string, { translationKey: string; english: string }>> = catalog.messages;

/** Presentation only: the persisted message remains the authoritative reason. */
export function runFailureDetail(reason: string): string {
  const entry = Object.hasOwn(messages, reason) ? messages[reason] : undefined;
  if (/^(?:model|provider|prepared_prompt|completion)_[a-z0-9_]+$/.test(reason)) {
    const label = entry && entry.translationKey !== "modelErrors.failed" ? entry.english : catalog.failureLabel;
    return `${label} · ${reason}`;
  }
  return entry?.english ?? reason;
}
