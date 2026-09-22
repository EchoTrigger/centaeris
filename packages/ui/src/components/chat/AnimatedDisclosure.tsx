import { useLayoutEffect, useRef, useState, type ReactNode } from "react";

// Mount lazily, retain content during closure, and release readers afterwards.
// Initial/history renders settle immediately; only a changed disclosure animates.
export function AnimatedDisclosure({ expanded, children, animateHeight = true }: { expanded: boolean; children: ReactNode; animateHeight?: boolean }) {
  const [present, setPresent] = useState(expanded);
  const elementRef = useRef<HTMLDivElement>(null);
  const previousExpanded = useRef(expanded);
  const animationRef = useRef<Animation | null>(null);
  if (expanded && !present) setPresent(true);

  useLayoutEffect(() => {
    const element = elementRef.current;
    if (!element) { setPresent(expanded); return; }
    if (previousExpanded.current === expanded) return;
    previousExpanded.current = expanded;
    const previous = animationRef.current;
    const height = previous ? element.getBoundingClientRect().height : expanded ? 0 : element.scrollHeight;
    const opacity = previous ? getComputedStyle(element).opacity : expanded ? "0" : "1";
    previous?.cancel();
    const preference = window.matchMedia("(prefers-reduced-motion: reduce)");
    const finish = () => {
      animationRef.current = null;
      setPresent(expanded);
    };
    if (!animateHeight || preference.matches || element.scrollHeight > 1200) { finish(); return; }
    const animation = element.animate([
      { height: `${height}px`, opacity },
      { height: expanded ? `${element.scrollHeight}px` : "0px", opacity: expanded ? 1 : 0 },
    ], { duration: 180, easing: "cubic-bezier(.2, 0, 0, 1)" });
    animationRef.current = animation;
    void animation.finished.then(() => {
      if (animationRef.current === animation) finish();
    }, () => { /* Cancellation leaves completion to the replacement animation. */ });
    const reduce = () => { if (preference.matches) animation.finish(); };
    preference.addEventListener("change", reduce);
    return () => { preference.removeEventListener("change", reduce); };
  }, [expanded, animateHeight]);

  useLayoutEffect(() => () => {
    if (animationRef.current) {
      animationRef.current.cancel();
      animationRef.current = null;
    }
  }, []);

  return <div className="chatDisclosure" ref={elementRef} inert={!expanded} aria-hidden={!expanded}
    hidden={!present}>{present ? children : null}</div>;
}
