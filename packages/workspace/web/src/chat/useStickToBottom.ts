import { useLayoutEffect, useRef } from "react";

const BOTTOM_TOLERANCE_PX = 24;

// Keeps a scrollable element pinned to its bottom while its content grows, and
// detaches once the user scrolls away from the bottom. The subtree is watched
// so streaming text growth (which does not resize the box) still follows.
// `active` re-arms following when the container becomes visible, e.g. expanded.
export function useStickToBottom<T extends HTMLElement>(active: boolean) {
  const ref = useRef<T>(null);
  const followingRef = useRef(true);
  useLayoutEffect(() => {
    if (!active) return undefined;
    const element = ref.current;
    if (!element) return undefined;
    followingRef.current = true;
    const stick = () => {
      if (!followingRef.current) return;
      if (element.scrollTop >= element.scrollHeight - element.clientHeight) return;
      element.scrollTop = element.scrollHeight;
    };
    element.scrollTop = element.scrollHeight;
    const observer = new MutationObserver(stick);
    observer.observe(element, { childList: true, subtree: true, characterData: true });
    const onScroll = () => {
      followingRef.current =
        element.scrollHeight - element.clientHeight - element.scrollTop <= BOTTOM_TOLERANCE_PX;
    };
    element.addEventListener("scroll", onScroll, { passive: true });
    // Content can settle after the first layout (async reference reads, fonts,
    // markdown). Re-stick on the next frame as well.
    const frame = window.requestAnimationFrame(stick);
    return () => {
      window.cancelAnimationFrame(frame);
      observer.disconnect();
      element.removeEventListener("scroll", onScroll);
    };
  }, [active]);
  return ref;
}
