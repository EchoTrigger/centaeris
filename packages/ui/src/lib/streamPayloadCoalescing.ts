export type StreamPayloadRecord = Record<string, unknown>;

export type OrderedStreamBuffer<TPayload extends object> = {
  items: TPayload[];
  cursor: number;
};

const readRecord = (value: unknown): StreamPayloadRecord | null =>
  typeof value === "object" && value !== null
    ? value as StreamPayloadRecord
    : null;

const readString = (record: StreamPayloadRecord | null, key: string): string =>
  typeof record?.[key] === "string" ? record[key].trim() : "";

const readRawString = (record: StreamPayloadRecord | null, key: string): string =>
  typeof record?.[key] === "string" ? record[key] : "";

const eventCoalescingKey = (payload: unknown): string | null => {
  const payloadRecord = readRecord(payload);
  if (readString(payloadRecord, "type") !== "runtime_event") {
    return null;
  }
  const event = readRecord(payloadRecord?.event);
  const eventType = readString(event, "type");
  if (
    ![
      "ModelTextDelta",
      "ModelTextReplace",
      "ModelStatus",
      "Status",
      "ToolProgress",
    ].includes(eventType)
  ) {
    return null;
  }
  const eventPayload = readRecord(event?.payload);
  return [
    eventType,
    readString(event, "sessionId"),
    readString(event, "turnId"),
    readString(event, "taskId"),
    readString(eventPayload, "callId"),
  ].join(":");
};

const mergeAdjacentPayloads = <TPayload extends object>(
  previous: TPayload,
  next: TPayload,
): TPayload | null => {
  const previousKey = eventCoalescingKey(previous);
  if (!previousKey || previousKey !== eventCoalescingKey(next)) {
    return null;
  }
  const nextRecord = readRecord(next);
  const nextEvent = readRecord(nextRecord?.event);
  const nextEventPayload = readRecord(nextEvent?.payload);
  const eventType = readString(nextEvent, "type");
  if (eventType !== "ModelTextDelta") {
    return next;
  }
  const previousRecord = readRecord(previous);
  const previousEvent = readRecord(previousRecord?.event);
  const previousEventPayload = readRecord(previousEvent?.payload);
  const previousDelta = readRawString(previousEventPayload, "delta");
  const nextDelta = readRawString(nextEventPayload, "delta");
  if (!previousDelta || !nextDelta || !nextEvent || !nextEventPayload) {
    return next;
  }
  return {
    ...next,
    event: {
      ...nextEvent,
      payload: {
        ...nextEventPayload,
        delta: previousDelta + nextDelta,
      },
    },
  } as TPayload;
};

export const compactConsumedStreamPayloads = <
  TPayload extends object,
>(buffer: OrderedStreamBuffer<TPayload>): void => {
  if (buffer.cursor === 0) {
    return;
  }
  if (buffer.cursor >= buffer.items.length) {
    buffer.items.length = 0;
    buffer.cursor = 0;
    return;
  }
  if (buffer.cursor >= 1024 && buffer.cursor * 2 >= buffer.items.length) {
    buffer.items.splice(0, buffer.cursor);
    buffer.cursor = 0;
  }
};

export const appendCoalescedStreamPayload = <
  TPayload extends object,
>(buffer: OrderedStreamBuffer<TPayload>, payload: TPayload): void => {
  compactConsumedStreamPayloads(buffer);
  const previousIndex = buffer.items.length - 1;
  if (previousIndex >= buffer.cursor) {
    const merged = mergeAdjacentPayloads(buffer.items[previousIndex], payload);
    if (merged) {
      buffer.items[previousIndex] = merged;
      return;
    }
  }
  buffer.items.push(payload);
};

export const coalesceStreamPayloadSequence = <
  TPayload extends object,
>(payloads: readonly TPayload[]): TPayload[] => {
  const buffer: OrderedStreamBuffer<TPayload> = { items: [], cursor: 0 };
  for (const payload of payloads) {
    appendCoalescedStreamPayload(buffer, payload);
  }
  return buffer.items;
};
