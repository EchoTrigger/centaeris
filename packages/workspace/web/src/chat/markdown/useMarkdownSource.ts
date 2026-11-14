import { useEffect, useLayoutEffect, useRef, useState } from "react";

// Combine live updates within one animation frame without changing source text.
export function useMarkdownSource(source: string, streaming: boolean) {
  const [visible, setVisible] = useState(source);
  const pending = useRef(source);
  const frame = useRef(0);
  useLayoutEffect(() => {
    pending.current = source;
    if (!streaming || document.hidden || typeof requestAnimationFrame !== "function") {
      if (frame.current) cancelAnimationFrame(frame.current);
      frame.current = 0;
      setVisible(source);
      return;
    }
    if (!frame.current) frame.current = requestAnimationFrame(() => {
      frame.current = 0;
      setVisible(pending.current);
    });
  }, [source, streaming]);
  useEffect(() => () => { if (frame.current) cancelAnimationFrame(frame.current); }, []);
  return !streaming || !source.startsWith(visible) ? source : visible;
}
