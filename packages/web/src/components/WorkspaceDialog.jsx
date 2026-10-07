
import { useTranslation } from "../i18n";
import { useId } from "react";
import { X } from "lucide-react";
import { useModalDialog } from "./useModalDialog";

export function WorkspaceDialog({ title, description, children, busy = false, wide = false, onClose }) {
  const { t } = useTranslation();
  const titleId = useId();
  const dialogRef = useModalDialog({ busy, onClose });
  return <div className="themeConfirmBackdrop" role="presentation" onMouseDown={() => !busy && onClose()}>
    <section className={`themeConfirmDialog shWorkspaceDialog ${wide ? "isWide" : ""}`} ref={dialogRef} role="dialog" aria-modal="true" aria-labelledby={titleId} tabIndex={-1} onMouseDown={(event) => event.stopPropagation()}>
      <header>
        <div><h2 id={titleId}>{title}</h2>{description ? <p>{description}</p> : null}</div>
        <button type="button" disabled={busy} aria-label={t("workspaceContextPanel.close")} onClick={onClose}><X aria-hidden="true" /></button>
      </header>
      {children}
    </section>
  </div>;
}
