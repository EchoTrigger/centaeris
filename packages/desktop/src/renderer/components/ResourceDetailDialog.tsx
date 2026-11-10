import type { ReactNode } from "react";
import { X } from "lucide-react";
import { Button } from "./ui/button";
import { Dialog, DialogClose, DialogContent } from "./ui/dialog";

export function ResourceDetailDialog({ title, onClose, children }: { title: string; onClose: () => void; children: ReactNode }) {
  return <Dialog open onOpenChange={(next) => { if (!next) onClose(); }}>
    <DialogContent className="resourceDetailDialog" aria-label={title} showCloseButton={false}>
      <DialogClose render={<Button variant="ghost" size="icon-sm" className="resourceDetailClose" aria-label="Close detail" />}><X size={18} aria-hidden="true" /></DialogClose>
      {children}
    </DialogContent>
  </Dialog>;
}
