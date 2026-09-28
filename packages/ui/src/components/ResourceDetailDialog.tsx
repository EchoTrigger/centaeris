import { useEffect, useRef, type ReactNode } from "react";
import { X } from "lucide-react";
export function ResourceDetailDialog({ title, onClose, children }: { title: string; onClose: () => void; children: ReactNode }) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = ref.current;
    const previous = typeof document !== "undefined" ? document.activeElement : null;
    dialog?.showModal();
    return () => { dialog?.close(); if (typeof HTMLElement !== "undefined" && previous instanceof HTMLElement && previous.isConnected) previous.focus(); };
  }, []);
  return <dialog ref={ref} className="resourceDetailDialog" aria-label={title} onCancel={event => { event.preventDefault(); onClose(); }}>
    <button type="button" className="resourceDetailClose" aria-label="Close detail" onClick={onClose}><X size={18} /></button>
    {children}
  </dialog>;
}
