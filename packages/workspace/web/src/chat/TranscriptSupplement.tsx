import type { ReactNode } from "react";
import { Redo2 } from "lucide-react";
import { useTranslation } from "../i18n";

export function TranscriptSupplement({ children, blockId }: Readonly<{
  children: ReactNode;
  blockId?: string;
}>) {
  const { t } = useTranslation();
  return <div className="workspaceTranscriptBlock workspaceTranscriptUser workspaceSupplement" data-block-id={blockId}>
    <div className="workspaceSupplementHeading"><Redo2 aria-hidden="true" /><span>{t("chat.steer")}</span></div>
    <div className="workspaceUserMessage">{children}</div>
  </div>;
}
