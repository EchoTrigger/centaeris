import type { AgentMessageView } from "./agentChatViewTypes";

export const AGENT_CHAT_TIME_GAP_MS = 20 * 60 * 1000;

function absoluteInstant(value: string | undefined) {
  // A display clock must never fill a missing canonical message/receipt instant.
  if (!value || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/.test(value)) return null;
  const date = new Date(value);
  if (!Number.isFinite(date.getTime())) return null;
  const calendarDate = new Date(`${value.slice(0, 10)}T00:00:00Z`);
  if (calendarDate.toISOString().slice(0, 10) !== value.slice(0, 10) || Number(value.slice(11, 13)) > 23) return null;
  return date;
}

export function formatSuppliedTime(value: string | undefined, locale?: string, timeZone?: string) {
  const date = absoluteInstant(value);
  if (!date || !value) return null;
  return {
    instant: value,
    label: new Intl.DateTimeFormat(locale, { hour: "2-digit", minute: "2-digit", timeZone }).format(date),
    full: new Intl.DateTimeFormat(locale, { dateStyle: "full", timeStyle: "long", timeZone }).format(date),
  };
}

export function agentChatTimeline(messages: readonly AgentMessageView[], referenceTime: string, locale?: string, timeZone?: string) {
  // No timeZone/locale override means the browser's local display settings.
  const dayFormat = new Intl.DateTimeFormat("en-US-u-ca-gregory-nu-latn", { year: "numeric", month: "numeric", day: "numeric", timeZone });
  const localDay = (date: Date) => {
    const parts = dayFormat.formatToParts(date);
    const part = (type: Intl.DateTimeFormatPartTypes) => Number(parts.find(value => value.type === type)!.value);
    const calendar = new Date(0);
    calendar.setUTCFullYear(part("year"), part("month") - 1, part("day"));
    return Math.floor(calendar.getTime() / 86400000);
  };
  const reference = absoluteInstant(referenceTime);
  const referenceDay = reference ? localDay(reference) : null;
  const dateFormat = new Intl.DateTimeFormat(locale, { dateStyle: "medium", timeZone });
  const clockFormat = new Intl.DateTimeFormat(locale, { hour: "numeric", minute: "2-digit", hourCycle: "h12", timeZone });
  const fullFormat = new Intl.DateTimeFormat(locale, { dateStyle: "full", timeStyle: "long", timeZone });
  const relativeFormat = new Intl.RelativeTimeFormat(locale, { numeric: "auto" });
  const english = /^en(?:-|$)/.test(relativeFormat.resolvedOptions().locale);
  let previous: Date | null = null;
  return messages.map(message => {
    const date = absoluteInstant(message.createdAt);
    const day = date ? localDay(date) : null;
    const beginsGroup = date && (!previous || date.getTime() - previous.getTime() >= AGENT_CHAT_TIME_GAP_MS);
    previous = date;
    if (!beginsGroup || !date || !message.createdAt) return { message, separator: null };
    const difference = referenceDay === null || day === null ? null : day - referenceDay;
    let dayLabel = difference === 0 || difference === -1 ? relativeFormat.format(difference, "day") : dateFormat.format(date);
    if (english) dayLabel = dayLabel[0].toLocaleUpperCase(locale) + dayLabel.slice(1);
    const parts = clockFormat.formatToParts(date);
    const part = (type: Intl.DateTimeFormatPartTypes) => parts.find(value => value.type === type)!.value;
    const period = english ? part("dayPeriod").toLocaleUpperCase(locale) : part("dayPeriod");
    return { message, separator: {
      instant: message.createdAt,
      // Fixed product order even when Intl's localized string puts 下午 first.
      label: `${dayLabel} ${part("hour")}:${part("minute")} ${period}`,
      full: fullFormat.format(date),
    } };
  });
}
