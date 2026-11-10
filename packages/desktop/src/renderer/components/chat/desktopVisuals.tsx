import { lazy } from "react";
import type { VisualEnvelope } from "@centaeris/visuals/contract";

const DesktopVisualRenderer = lazy(() => import("./DesktopVisualRenderer"));

export function renderDesktopVisual(visual: VisualEnvelope) {
  return <DesktopVisualRenderer visual={visual} />;
}
