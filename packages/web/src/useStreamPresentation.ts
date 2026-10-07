import { useEffect, useLayoutEffect, useRef, useState } from "react";

// Presentation only: keep durable/model text untouched and cap each live burst at 120ms.
export function useStreamPresentation(text: string, live: boolean) {
  const available = typeof window !== "undefined" && typeof window.requestAnimationFrame === "function" && typeof window.matchMedia === "function";
  const initial = available && live && !window.matchMedia("(prefers-reduced-motion: reduce)").matches ? text.slice(0, 24) : text;
  const [visible, setVisible] = useState(initial);
  const state = useRef({ shown: initial, target: text, frame: 0, deadline: 0, previous: 0 });
  useLayoutEffect(() => {
    const current = state.current;
    const flush = () => {
      if (current.frame) cancelAnimationFrame(current.frame);
      current.frame = 0;
      current.shown = text;
      current.target = text;
      setVisible(text);
    };
    if (!available || !live || document.hidden || matchMedia("(prefers-reduced-motion: reduce)").matches || !text.startsWith(current.shown)) {
      flush(); return;
    }
    if (current.shown === text) return;
    if (!current.frame) {
      current.previous = performance.now();
      current.deadline = current.previous + 120;
    }
    current.target = text;
    const tick = (now: number) => {
      const remaining = current.target.length - current.shown.length;
      const duration = Math.max(1, current.deadline - current.previous);
      const elapsed = Math.max(1, now - current.previous);
      let end = Math.min(current.target.length, current.shown.length + Math.max(1, Math.ceil(remaining * elapsed / duration)));
      const code = current.target.charCodeAt(end - 1);
      if (code >= 0xd800 && code <= 0xdbff && end < current.target.length) end += 1;
      current.shown = current.target.slice(0, end);
      current.previous = now;
      setVisible(current.shown);
      current.frame = end < current.target.length ? requestAnimationFrame(tick) : 0;
    };
    if (!current.frame) current.frame = requestAnimationFrame(tick);
  }, [available, live, text]);
  useEffect(() => {
    const current = state.current;
    const flushWhenHidden = () => {
      if (!document.hidden) return;
      if (current.frame) cancelAnimationFrame(current.frame);
      current.frame = 0;
      current.shown = current.target;
      setVisible(current.shown);
    };
    if (available) document.addEventListener("visibilitychange", flushWhenHidden);
    return () => {
      if (current.frame) cancelAnimationFrame(current.frame);
      if (available) document.removeEventListener("visibilitychange", flushWhenHidden);
    };
  }, [available]);
  // Corrections, history and terminal state must never show stale or partial content.
  return !available || !live || !text.startsWith(visible) ? text : visible;
}
