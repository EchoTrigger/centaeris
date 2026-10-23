import { useAnimate } from "motion/react";
import { useLayoutEffect, useRef, useState, type ReactNode } from "react";

// Mount lazily, retain closing readers until Motion settles, then release them.
// Initial/history renders stay still; reversing an interaction resumes in place.
export function AnimatedDisclosure({ expanded, children, animateHeight = true }: { expanded: boolean; children: ReactNode; animateHeight?: boolean }) {
  const [present, setPresent] = useState(expanded);
  const [scope, animate] = useAnimate<HTMLDivElement>();
  const previousExpanded = useRef(expanded);
  const initialStyle = useRef({ height: expanded ? "auto" : "0px", opacity: expanded ? 1 : 0 });
  if (expanded && !present) setPresent(true);

  useLayoutEffect(() => {
    const element = scope.current;
    const changed = previousExpanded.current !== expanded;
    previousExpanded.current = expanded;
    if (!element) { setPresent(expanded); return; }
    const settle = () => {
      // Expanded output can continue streaming after the interaction completes.
      element.style.height = expanded ? "auto" : "0px";
      element.style.opacity = expanded ? "1" : "0";
      setPresent(expanded);
    };
    const preference = window.matchMedia("(prefers-reduced-motion: reduce)");
    if (!changed || !animateHeight || preference.matches || element.scrollHeight > 1200) {
      settle();
      return;
    }
    const playback = animate(element,
      { height: expanded ? "auto" : 0, opacity: expanded ? 1 : 0 },
      { duration: 0.18, ease: [0.2, 0, 0, 1], reduceMotion: false });
    let current = true;
    const reduce = () => {
      if (!preference.matches) return;
      current = false;
      preference.removeEventListener("change", reduce);
      playback.stop();
      settle();
    };
    preference.addEventListener("change", reduce);
    void playback.then(() => {
      if (!current) return;
      current = false;
      preference.removeEventListener("change", reduce);
      settle();
    });
    return () => {
      preference.removeEventListener("change", reduce);
      current = false;
      // Motion retains the current visual value so a reversal does not jump.
      playback.stop();
    };
  }, [expanded, animateHeight, animate, scope]);

  return <div className="chatDisclosure" ref={scope} inert={!expanded} aria-hidden={!expanded}
    hidden={!present} style={initialStyle.current}>{present ? children : null}</div>;
}
