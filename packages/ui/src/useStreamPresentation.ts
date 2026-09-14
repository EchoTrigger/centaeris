import { useEffect, useLayoutEffect, useRef, useState } from "react";

const STREAM_PRESENTATION_FRAME_MS = 1_000 / 60;
const STREAM_PRESENTATION_TARGET_LAG_MS = 150;
const STREAM_PRESENTATION_TERMINAL_DRAIN_MS = 200;
const STREAM_PRESENTATION_MIN_GRAPHEMES_PER_SECOND = 60;
const STREAM_PRESENTATION_MAX_GRAPHEMES_PER_SECOND = 720;
const STREAM_PRESENTATION_MAX_GRAPHEMES_PER_FRAME = 12;
const STREAM_PRESENTATION_FIRST_PAINT_GRAPHEMES = 1;

const graphemeSegmenter =
  typeof Intl !== "undefined" && "Segmenter" in Intl
    ? new Intl.Segmenter(undefined, { granularity: "grapheme" })
    : null;

const advanceGraphemes = (
  text: string,
  start: number,
  requestedCount: number,
): number => {
  const count = Math.max(0, Math.floor(requestedCount));
  if (count === 0 || start >= text.length) {
    return start;
  }
  const suffix = text.slice(start);
  let advanced = 0;
  let end = start;
  if (graphemeSegmenter) {
    for (const part of graphemeSegmenter.segment(suffix)) {
      end = start + part.index + part.segment.length;
      advanced += 1;
      if (advanced >= count) {
        break;
      }
    }
    return end;
  }
  for (const part of suffix) {
    end += part.length;
    advanced += 1;
    if (advanced >= count) {
      break;
    }
  }
  return end;
};

const initialPresentationText = (text: string, animate: boolean): string =>
  animate
    ? text.slice(
        0,
        advanceGraphemes(
          text,
          0,
          STREAM_PRESENTATION_FIRST_PAINT_GRAPHEMES,
        ),
      )
    : text;

type StreamPresentationState = {
  shown: string;
  target: string;
  frame: number;
  previousFrameAt: number | null;
  fractionalGraphemes: number;
  wasLive: boolean;
  terminalDrainPending: boolean;
  terminalDeadline: number | null;
};

// Presentation only: durable/model text remains authoritative and is never rewritten.
export function useStreamPresentation(text: string, live: boolean) {
  const available =
    typeof window !== "undefined" &&
    typeof document !== "undefined" &&
    typeof window.requestAnimationFrame === "function" &&
    typeof window.matchMedia === "function";
  const animateInitially =
    available &&
    live &&
    !document.hidden &&
    !window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const [visible, setVisible] = useState(() =>
    initialPresentationText(text, animateInitially),
  );
  const state = useRef<StreamPresentationState>({
    shown: initialPresentationText(text, animateInitially),
    target: text,
    frame: 0,
    previousFrameAt: null,
    fractionalGraphemes: 0,
    wasLive: live,
    terminalDrainPending: false,
    terminalDeadline: null,
  });

  useLayoutEffect(() => {
    const current = state.current;
    const flush = () => {
      if (current.frame) cancelAnimationFrame(current.frame);
      current.frame = 0;
      current.shown = text;
      current.target = text;
      current.previousFrameAt = null;
      current.fractionalGraphemes = 0;
      current.wasLive = live;
      current.terminalDrainPending = false;
      current.terminalDeadline = null;
      setVisible(text);
    };
    if (
      !available ||
      document.hidden ||
      window.matchMedia("(prefers-reduced-motion: reduce)").matches ||
      !text.startsWith(current.shown)
    ) {
      flush();
      return;
    }
    current.target = text;
    if (live) {
      current.wasLive = true;
      current.terminalDrainPending = false;
      current.terminalDeadline = null;
    } else if (current.wasLive) {
      current.wasLive = false;
      current.terminalDrainPending = current.shown !== text;
      current.terminalDeadline = null;
    } else if (current.terminalDeadline === null) {
      flush();
      return;
    }
    if (current.shown === text) {
      return;
    }

    const tick = (now: number) => {
      current.frame = 0;
      if (!current.target.startsWith(current.shown)) {
        current.shown = current.target;
        setVisible(current.target);
        return;
      }
      const remaining = current.target.length - current.shown.length;
      if (remaining <= 0) {
        current.previousFrameAt = now;
        return;
      }
      if (current.terminalDrainPending) {
        current.terminalDrainPending = false;
        current.terminalDeadline = now + STREAM_PRESENTATION_TERMINAL_DRAIN_MS;
      }
      const elapsed = Math.max(
        1,
        current.previousFrameAt === null
          ? STREAM_PRESENTATION_FRAME_MS
          : now - current.previousFrameAt,
      );
      current.previousFrameAt = now;
      let requestedGraphemes: number;
      if (current.terminalDeadline !== null) {
        if (now >= current.terminalDeadline) {
          current.shown = current.target;
          current.terminalDeadline = null;
          setVisible(current.shown);
          return;
        }
        const framesRemaining = Math.max(
          1,
          Math.ceil(
            (current.terminalDeadline - now) /
              STREAM_PRESENTATION_FRAME_MS,
          ),
        );
        requestedGraphemes = Math.max(1, Math.ceil(remaining / framesRemaining));
      } else {
        const targetRate = Math.min(
          STREAM_PRESENTATION_MAX_GRAPHEMES_PER_SECOND,
          Math.max(
            STREAM_PRESENTATION_MIN_GRAPHEMES_PER_SECOND,
            (remaining * 1_000) / STREAM_PRESENTATION_TARGET_LAG_MS,
          ),
        );
        current.fractionalGraphemes += (targetRate * elapsed) / 1_000;
        requestedGraphemes = Math.min(
          STREAM_PRESENTATION_MAX_GRAPHEMES_PER_FRAME,
          Math.max(1, Math.floor(current.fractionalGraphemes)),
        );
        current.fractionalGraphemes = Math.max(
          0,
          current.fractionalGraphemes - requestedGraphemes,
        );
      }
      const end = advanceGraphemes(
        current.target,
        current.shown.length,
        requestedGraphemes,
      );
      current.shown = current.target.slice(0, end);
      setVisible(current.shown);
      if (end < current.target.length) {
        current.frame = requestAnimationFrame(tick);
      } else {
        current.terminalDeadline = null;
      }
    };
    if (!current.frame) {
      current.frame = requestAnimationFrame(tick);
    }
  }, [available, live, text]);

  useEffect(() => {
    const current = state.current;
    const flushWhenHidden = () => {
      if (!document.hidden) return;
      if (current.frame) cancelAnimationFrame(current.frame);
      current.frame = 0;
      current.shown = current.target;
      current.previousFrameAt = null;
      current.fractionalGraphemes = 0;
      current.wasLive = false;
      current.terminalDrainPending = false;
      current.terminalDeadline = null;
      setVisible(current.shown);
    };
    if (available) document.addEventListener("visibilitychange", flushWhenHidden);
    return () => {
      if (current.frame) cancelAnimationFrame(current.frame);
      if (available) document.removeEventListener("visibilitychange", flushWhenHidden);
    };
  }, [available]);

  // Corrections and history never show stale content; append-only terminal text may drain briefly.
  return !available || !text.startsWith(visible) ? text : visible;
}
