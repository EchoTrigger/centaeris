import { useLayoutEffect, type ReactNode } from "react";
import { motion, useMotionValue, useReducedMotion } from "@centaeris/frontend-foundations/motion-react";

export type ComposerPlaneProps = { welcome: boolean; animateDocking: boolean; children: ReactNode };

export function ComposerPlane({ welcome, animateDocking, children }: ComposerPlaneProps) {
  const reduceMotion = useReducedMotion();
  const top = useMotionValue(welcome ? "50%" : "100%");
  const y = useMotionValue(welcome ? "-50%" : "-100%");
  useLayoutEffect(() => {
    if (welcome || animateDocking) return;
    // A navigation can retain the same target as the previous owner's spring.
    // Jump stops that animation even when its declarative target hasn't changed.
    top.jump("100%");
    y.jump("-100%");
  }, [welcome, animateDocking, top, y]);
  return <motion.div
    role="region"
    aria-label="Message composer"
    data-position={welcome ? "center" : "bottom"}
    className={"chatBottomPlane " + (welcome ? "is-welcome" : "")}
    initial={false}
    style={{ top, y }}
    animate={{ top: welcome ? "50%" : "100%", y: welcome ? "-50%" : "-100%" }}
    transition={reduceMotion || welcome || !animateDocking ? { duration: 0 } : { type: "spring", visualDuration: 0.6, bounce: 0 }}
  >{children}</motion.div>;
}
