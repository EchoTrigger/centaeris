import catalog from "../../../../assets/run-error-messages.json";

const messages: Readonly<Record<string, { translationKey: string; english: string }>> = catalog.messages;

function failureLabel(value?: string): string | undefined {
  return value && Object.hasOwn(messages, value) ? messages[value]?.english : undefined;
}

export function runFailureDetail(message: string, reasonType?: string): string {
  return failureLabel(reasonType) ?? failureLabel(message) ?? messages.model_adapter_failed.english;
}
