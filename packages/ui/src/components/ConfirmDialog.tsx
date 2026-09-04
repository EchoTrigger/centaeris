import { useEffect, useId, useRef, type MouseEvent } from "react";

export type ConfirmationRequest = {
  title: string;
  message?: string;
};

export type ConfirmAction = (request: ConfirmationRequest) => Promise<boolean>;

type ConfirmDialogProps = ConfirmationRequest & {
  open: boolean;
  onCancel: () => void;
  onConfirm: () => void;
};

export function ConfirmDialog({
  open,
  title,
  message,
  onCancel,
  onConfirm,
}: ConfirmDialogProps) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  const messageId = useId();

  const cancelFromBackdrop = (event: MouseEvent<HTMLDialogElement>) => {
    const bounds = event.currentTarget.getBoundingClientRect();
    if (
      event.clientX < bounds.left
      || event.clientX > bounds.right
      || event.clientY < bounds.top
      || event.clientY > bounds.bottom
    ) onCancel();
  };

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    if (!open && dialog.open) dialog.close();
  }, [open]);

  return (
    <dialog
      ref={dialogRef}
      className="confirmDialog"
      role="alertdialog"
      aria-labelledby={titleId}
      aria-describedby={message ? messageId : undefined}
      onClick={cancelFromBackdrop}
      onCancel={(event) => {
        event.preventDefault();
        onCancel();
      }}
    >
      <h1 id={titleId}>{title}</h1>
      {message ? <p id={messageId}>{message}</p> : null}
      <footer>
        <button type="button" autoFocus onClick={onCancel}>Cancel</button>
        <button type="button" className="is-confirm" onClick={onConfirm}>Confirm</button>
      </footer>
    </dialog>
  );
}
