import { useRef } from "react";
import { Button } from "./ui/button";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "./ui/dialog";

export type ConfirmationRequest = { title: string; message?: string };
export type ConfirmAction = (request: ConfirmationRequest) => Promise<boolean>;
type ConfirmDialogProps = ConfirmationRequest & { open: boolean; onCancel: () => void; onConfirm: () => void };

export function ConfirmDialog({ open, title, message, onCancel, onConfirm }: ConfirmDialogProps) {
  const cancel = useRef<HTMLButtonElement>(null);
  return <Dialog open={open} onOpenChange={(next) => { if (!next) onCancel(); }}>
    <DialogContent role="alertdialog" showCloseButton={false} initialFocus={cancel}>
      <DialogHeader><DialogTitle>{title}</DialogTitle>
        {message ? <DialogDescription>{message}</DialogDescription> : null}
      </DialogHeader>
      <DialogFooter>
        <DialogClose render={<Button ref={cancel} variant="ghost" />}>Cancel</DialogClose>
        <Button variant="solid" type="button" onClick={onConfirm}>Confirm</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>;
}
