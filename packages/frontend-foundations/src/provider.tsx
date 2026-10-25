import type { ReactNode } from "react";
import { LucideProvider } from "lucide-react";
import { MotionConfig } from "motion/react";

// Hosts keep their existing appearance; the provider owns platform preferences.
export function FrontendProvider({ children, iconStrokeWidth = 1.5 }: {
  children: ReactNode;
  iconStrokeWidth?: number;
}) {
  return <LucideProvider strokeWidth={iconStrokeWidth}>
    <MotionConfig reducedMotion="user">{children}</MotionConfig>
  </LucideProvider>;
}
