import { useEffect } from "react";

export function ScrollActivity() {
  useEffect(() => {
    const timers = new Map<HTMLElement, ReturnType<typeof setTimeout>>();
    const onScroll = (event: Event) => {
      const element = event.target === document ? document.scrollingElement : event.target;
      if (!(element instanceof HTMLElement)) return;
      const previous = timers.get(element);
      if (previous) clearTimeout(previous);
      element.dataset.scrollActive = "true";
      timers.set(element, setTimeout(() => {
        delete element.dataset.scrollActive;
        timers.delete(element);
      }, 700));
    };
    document.addEventListener("scroll", onScroll, { capture: true, passive: true });
    return () => {
      document.removeEventListener("scroll", onScroll, true);
      for (const [element, timer] of timers) { clearTimeout(timer); delete element.dataset.scrollActive; }
    };
  }, []);
  return null;
}
