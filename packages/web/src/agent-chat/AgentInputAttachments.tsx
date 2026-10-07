import { useCallback, useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { FileOutput, X } from "lucide-react";
import { apiUrl } from "../api";
import { useTranslation } from "../i18n";
import { AttachmentCard } from "../chat/AttachmentCard";
import { DocumentPreview } from "../components/DocumentPreview";
import { useModalDialog } from "../components/useModalDialog";
import { officeFileType } from "../chat/officeFormats.mjs";
import { codePreviewCanRender } from "../chat/codePreviewFormats.mjs";
import type { AgentInputAttachment } from "./preparedAgentInput";
import "./agent-attachments.css";

export function AgentInputAttachments({ attachments, agentId, inputId, onRemove }: Readonly<{ attachments: readonly AgentInputAttachment[]; agentId: string; inputId?: string; onRemove?: (inputRef: string) => void }>) {
  const { t, i18n } = useTranslation();
  const [preview, setPreview] = useState<AgentInputAttachment | null>(null);
  const [imageUrl, setImageUrl] = useState(""); const [error, setError] = useState(false);
  const dialogRef = useModalDialog({ open: Boolean(preview), onClose: () => setPreview(null) });
  const base = `/api/agents/${encodeURIComponent(agentId)}${inputId ? `/inputs/${encodeURIComponent(inputId)}` : ""}/attachments`;
  const url = (attachment: AgentInputAttachment, action: "preview" | "download") => apiUrl(`${base}/${encodeURIComponent(attachment.inputRef)}/${action}?lang=${i18n.language === "en" ? "en" : "zh-CN"}`);
  const previewUrl = preview ? url(preview, "preview") : "";
  const image = Boolean(preview?.contentType.startsWith("image/"));
  const binary = image || preview?.contentType === "application/pdf";
  const unavailable = useCallback(() => { setError(true); }, []);
  useEffect(() => {
    setImageUrl(""); setError(false);
    if (!previewUrl || !binary) return;
    const controller = new AbortController(); let objectUrl = "";
    void fetch(previewUrl, { credentials: "include", cache: "no-store", signal: controller.signal }).then(async response => {
      const mime = response.headers.get("Content-Type")?.split(";", 1)[0].toLowerCase();
      if (!response.ok || (image ? !mime?.startsWith("image/") : mime !== "application/pdf")) throw new Error("preview_unavailable");
      const blob = await response.blob(); if (controller.signal.aborted) return;
      objectUrl = URL.createObjectURL(blob); setImageUrl(objectUrl);
    }).catch(() => { if (!controller.signal.aborted) setError(true); });
    return () => { controller.abort(); if (objectUrl) URL.revokeObjectURL(objectUrl); };
  }, [previewUrl, image, binary]);
  return <><div className="workspaceComposerAttachments agentInputAttachmentList">{attachments.map(item => item.contentType.startsWith("image/")
    ? <AttachmentCard key={item.inputRef} attachment={item} imageUrl={url(item, "preview")} onPreview={() => setPreview(item)} onRemove={onRemove ? () => onRemove(item.inputRef) : undefined} />
    : <div className="agentInputAttachment" key={item.inputRef}><button className="agentInputAttachmentPreview" type="button" onClick={() => setPreview(item)} title={item.displayName}><FileOutput aria-hidden="true" /><span>{item.displayName}</span></button>{onRemove ? <button type="button" onClick={() => onRemove(item.inputRef)} aria-label={t("attachmentCard.removeValueFromThisMessage", { value1: item.displayName })}><X aria-hidden="true" /></button> : null}</div>)}</div>
    {preview ? createPortal(<div className="attachmentPreviewBackdrop"><section className="attachmentPreviewDialog" ref={dialogRef} role="dialog" aria-modal="true" aria-label={t("attachmentCard.previewValue", { value1: preview.displayName })} tabIndex={-1}>
      <header><strong>{preview.displayName}</strong><button type="button" onClick={() => setPreview(null)} aria-label={t("attachmentCard.closePreview")}><X aria-hidden="true" /></button></header>
      <a href={url(preview, "download")}>{t("agentChat.downloadFile")}</a>
      {error ? <p role="alert">{t("agentChat.filesError")}</p> : binary ? imageUrl ? image ? <img src={imageUrl} alt={preview.displayName} /> : <iframe src={imageUrl} title={preview.displayName} /> : null : officeFileType(preview.displayName) || preview.contentType.startsWith("text/") || codePreviewCanRender(preview.displayName, preview.contentType) ? <DocumentPreview src={previewUrl} title={preview.displayName} contentType={preview.contentType} onError={unavailable} /> : <p>{t("agentChat.filePreviewUnsupported")}</p>}
    </section></div>, document.body) : null}
  </>;
}
