import { lazy } from "react";
import type { VisualEnvelope } from "@centaeris/visualization-react/contract";

const DesktopVisualRenderer = lazy(() => import("./DesktopVisualRenderer"));

export function renderDesktopVisual(visual: VisualEnvelope) {
  return <DesktopVisualRenderer visual={visual} />;
}
