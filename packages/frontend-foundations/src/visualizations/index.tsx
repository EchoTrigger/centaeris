import { lazy } from "react";
import type { VisualEnvelope } from "@centaeris/visualization-react/contract";

const TranscriptVisualRenderer = lazy(() => import("./TranscriptVisualRenderer"));

export function renderTranscriptVisual(visual: VisualEnvelope) {
  return <TranscriptVisualRenderer visual={visual} />;
}
