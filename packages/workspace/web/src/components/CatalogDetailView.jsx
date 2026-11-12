import { useEffect, useRef } from "react";
import { Link } from "react-router";
import { ArrowLeft } from "lucide-react";
import { motion, useReducedMotion } from "@centaeris/ui/motion-react";

export function CatalogDetailView({ title, backTo, backLabel, children, hideTitle = false, className = "" }) {
  const reducedMotion = useReducedMotion();
  const heading = useRef(null);
  const origin = useRef(document.activeElement);
  useEffect(() => {
    heading.current?.focus({ preventScroll: true });
    const previous = origin.current;
    return () => { if (previous?.isConnected) previous.focus({ preventScroll: true }); };
  }, []);
  return <motion.section className={`catalogDetailView ${className}`} aria-label={title}
    ref={hideTitle ? heading : undefined} tabIndex={hideTitle ? -1 : undefined}
    initial={reducedMotion ? false : { opacity: 0 }} animate={{ opacity: 1 }} transition={{ duration: reducedMotion ? 0 : 0.16 }}>
    <Link className="catalogBack" to={backTo}><ArrowLeft aria-hidden="true" />{backLabel}</Link>
    {!hideTitle ? <h2 ref={heading} tabIndex={-1}>{title}</h2> : null}
    <div className="catalogDetailContent">{children}</div>
  </motion.section>;
}
