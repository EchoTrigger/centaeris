
import { useTranslation } from "../i18n";
import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { Check } from "lucide-react";
import { LoginForm } from "./LoginForm";
import { useModalDialog } from "./useModalDialog";

type SessionExpiredDialogProps = {
  open: boolean;
  user: { id: string; email: string } | undefined;
  onReauthenticated: () => void | Promise<void>;
  onContinue: () => void;
};

export function SessionExpiredDialog({ open, user, onReauthenticated, onContinue }: SessionExpiredDialogProps) {
  const { t } = useTranslation();
  const [restored, setRestored] = useState(false);
  const dialogRef = useModalDialog({ open, onClose: () => {} });

  useEffect(() => {
    if (!open) setRestored(false);
  }, [open]);

  if (!open || !user) return null;

  return createPortal(<div className="sessionExpiredBackdrop" role="presentation">
    <section className="sessionExpiredDialog" ref={dialogRef} role="dialog" aria-modal="true" aria-label={restored ? t("sessionExpiredDialog.youAreSignedInAgain") : t("sessionExpiredDialog.signInAgain")} tabIndex={-1}>
      {restored ? <div className="sessionExpiredRestored">
        <span aria-hidden="true"><Check /></span>
        <h1>{t("sessionExpiredDialog.youAreSignedInAgain")}</h1>
        <p>{t("sessionExpiredDialog.yourPageAndUnsavedChangesHaveBeenPreservedThe")}</p>
        <button autoFocus className="primary" type="button" onClick={onContinue}>{t("sessionExpiredDialog.continue")}</button>
      </div> : <LoginForm
        initialEmail={user.email}
        expectedUserId={user.id}
        heading={t("sessionExpiredDialog.sessionExpired")}
        description={t("sessionExpiredDialog.signInAgainToContinueYourWorkYourUnsaved")}
        submitLabel={t("sessionExpiredDialog.signInAgain")}
        onAuthenticated={async () => {
          await onReauthenticated();
          setRestored(true);
        }}
      />}
    </section>
  </div>, document.body);
}
