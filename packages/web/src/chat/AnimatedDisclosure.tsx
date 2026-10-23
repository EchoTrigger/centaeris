import { useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { animate } from "@centaeris/frontend-foundations/motion-mini";

// Mount lazily, retain content during closure, and release readers afterwards.
// Initial/history renders settle immediately; only a changed disclosure animates.
export function AnimatedDisclosure({ expanded, children }: { expanded: boolean; children: ReactNode }) {
  const [present, setPresent] = useState(expanded);
  const elementRef = useRef<HTMLDivElement>(null);
  const previousExpanded = useRef(expanded);
  const animationRef = useRef<ReturnType<typeof animate> | null>(null);
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
      const completedAnimation = animationRef.current;
      animationRef.current = null;
      // Keep the finished close clipped until React releases the children.
      // Cancel only after hiding, so the old content cannot repaint at full height.
      if (!expanded) element.hidden = true;
      setPresent(expanded);
      completedAnimation?.cancel();
      element.style.height = "";
      element.style.opacity = "";
    };
    if (preference.matches) { finish(); return; }
    const animation = animate(element, {
      height: [`${height}px`, expanded ? `${element.scrollHeight}px` : "0px"],
      opacity: [Number(opacity), expanded ? 1 : 0],
    }, { duration: 0.18, ease: [0.2, 0, 0, 1] });
    animationRef.current = animation;
    void animation.then(() => {
      if (animationRef.current === animation) finish();
    }, () => { /* Cancellation leaves completion to the replacement animation. */ });
    const reduce = () => { if (preference.matches) animation.complete(); };
    preference.addEventListener("change", reduce);
    return () => { preference.removeEventListener("change", reduce); };
  }, [expanded]);

  useLayoutEffect(() => () => {
    if (animationRef.current) {
      animationRef.current.cancel();
      animationRef.current = null;
    }
  }, []);

  return <div className="chatDisclosure" ref={elementRef} inert={!expanded} aria-hidden={!expanded}
    hidden={!present}>{present ? children : null}</div>;
}
