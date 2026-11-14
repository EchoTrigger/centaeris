
import { useTranslation } from "../i18n";
import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { ImageOff, X } from "lucide-react";
import { useModalDialog } from "../components/useModalDialog";
import { attachmentIsImage, attachmentPreviewUrl } from "./attachments.mjs";
import { filePresentation } from "./filePresentation.mjs";

export function AttachmentCard({ attachment, imageUrl, onPreview, onRemove, unavailable = false, className = "" }) {
  const { t } = useTranslation();
  const name = attachment.displayName;
  const image = attachmentIsImage(attachment);
  const source = image && !unavailable ? imageUrl ?? attachmentPreviewUrl(attachment) : "";
  const [failedSource, setFailedSource] = useState(null);
  const failed = unavailable || (Boolean(source) && failedSource === source);
  const { Icon, label } = filePresentation(name, attachment.contentType || attachment.asset?.contentType || "");
  const Content = onPreview && !unavailable ? "button" : "div";
  return (
    <div className={`attachmentCard ${className} ${image ? "isImage" : "isFile"} ${failed ? "isUnavailable" : ""}`}>
      <Content className="attachmentCardContent" aria-label={onPreview && !unavailable ? t("attachmentCard.previewValue", { value1: name }) : name} {...(Content === "button" ? { type: "button", onClick: onPreview } : {})}>
        {image ? source && !failed ? <img src={source} alt="" loading="lazy" decoding="async" onError={() => setFailedSource(source)} /> : <><ImageOff aria-hidden="true" /><small>{failed ? t("attachmentCard.imageUnavailable") : t("attachmentCard.image")}</small></> : <><Icon aria-hidden="true" /><small>{failed ? t("attachmentCard.unavailable") : label}</small><span className="attachmentCardName">{name}</span></>}
      </Content>
      {onRemove ? <button className="attachmentCardRemove" type="button" onClick={onRemove} aria-label={t("attachmentCard.removeValueFromThisMessage", { value1: name })}><X aria-hidden="true" /></button> : null}
    </div>
  );
}

const localFileKeys = new WeakMap();
let nextLocalFileKey = 0;
export function localAttachmentKey(file) {
  if (!localFileKeys.has(file)) localFileKeys.set(file, ++nextLocalFileKey);
  return localFileKeys.get(file);
}

export function LocalAttachmentCard({ file, onRemove }) {
  const { t } = useTranslation();
  const [imageUrl, setImageUrl] = useState("");
  const [previewOpen, setPreviewOpen] = useState(false);
  const dialogRef = useModalDialog({ open: previewOpen, onClose: () => setPreviewOpen(false) });
  useEffect(() => {
    if (!file.type.startsWith("image/")) return undefined;
    const url = URL.createObjectURL(file);
    setImageUrl(url);
    return () => URL.revokeObjectURL(url);
  }, [file]);
  return <>
    <AttachmentCard className="workspaceComposerAttachment" attachment={{ displayName: file.name, contentType: file.type }} imageUrl={imageUrl} onPreview={imageUrl ? () => setPreviewOpen(true) : undefined} onRemove={onRemove} />
    {previewOpen ? createPortal(<div className="attachmentPreviewBackdrop" role="presentation" onMouseDown={() => setPreviewOpen(false)}>
      <section className="attachmentPreviewDialog" ref={dialogRef} role="dialog" aria-modal="true" aria-label={t("attachmentCard.previewValue", { value1: file.name })} tabIndex={-1} onMouseDown={(event) => event.stopPropagation()}>
        <header><strong>{file.name}</strong><button type="button" onClick={() => setPreviewOpen(false)} aria-label={t("attachmentCard.closePreview")}><X aria-hidden="true" /></button></header>
        <img src={imageUrl} alt={file.name} decoding="async" />
      </section>
    </div>, document.body) : null}
  </>;
}
