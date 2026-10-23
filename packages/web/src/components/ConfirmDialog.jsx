import { t } from "../i18n";
import { useTranslation } from "../i18n";
import { useRef } from "react";
import { Dialog } from "@centaeris/frontend-foundations/primitives/dialog";

export function ConfirmDialog({
  open,
  title,
  message,
  confirmLabel = t("confirmDialog.confirm"),
  cancelLabel = t("agentRunRow.cancel"),
  busy = false,
  onCancel,
  onConfirm,
}) {
  const { t } = useTranslation();
  const cancel = useRef(null);

  if (!open) return null;
  return <Dialog.Root open={open} onOpenChange={(next, details) => {
    if (next) return;
    if (busy) details.cancel();
    else onCancel();
  }}>
    <Dialog.Portal>
      <Dialog.Backdrop className="themeConfirmBackdrop">
        <Dialog.Popup render={<section />} className="themeConfirmDialog" initialFocus={busy ? true : cancel}>
          <Dialog.Title render={<h2 />}>{title}</Dialog.Title>
          {message ? <Dialog.Description render={<p />}>{message}</Dialog.Description> : null}
          <footer>
            <button type="button" ref={cancel} disabled={busy} onClick={onCancel}>{cancelLabel}</button>
            <button type="button" className="isDanger" disabled={busy} onClick={onConfirm}>{busy ? t("confirmDialog.value", { value1: confirmLabel }) : confirmLabel}</button>
          </footer>
        </Dialog.Popup>
      </Dialog.Backdrop>
    </Dialog.Portal>
  </Dialog.Root>;
}
