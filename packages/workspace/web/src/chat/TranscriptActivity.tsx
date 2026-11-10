import { Loader, MessageCircleQuestion } from "lucide-react";
import { useTranslation } from "../i18n";
import type { PresentationActivity } from "./workspacePresentationProtocol";

export type TranscriptActivityState = Exclude<PresentationActivity["activity"], "preserving"> | "stopping";

export function TranscriptActivity({ activity = "working" }: Readonly<{ activity?: TranscriptActivityState }>) {
  const { t } = useTranslation();
  return <div className="workspaceTranscriptActivity" role="status" aria-live="polite">
    {activity === "waitingInput" ? <MessageCircleQuestion aria-hidden="true" /> : <Loader className="workspaceTranscriptActivityLoader" aria-hidden="true" />}
    <span>{t(`transcriptActivity.${activity ?? "working"}`)}</span>
  </div>;
}
