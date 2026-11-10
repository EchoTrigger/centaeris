import { useEffect, useRef } from "react";

const FOCUSABLE = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "[tabindex]:not([tabindex='-1'])",
].join(",");

function focusableElements(dialog) {
  return [...dialog.querySelectorAll(FOCUSABLE)].filter((element) => (
    element.tabIndex >= 0
    && element.getClientRects().length > 0
    && !element.closest("[inert]")
    && element.getAttribute("aria-hidden") !== "true"
  ));
}

function inertBackground(dialog) {
  const changed = [];
  for (let current = dialog; current?.parentElement && current !== document.body; current = current.parentElement) {
    for (const sibling of current.parentElement.children) {
      if (!(sibling instanceof HTMLElement) || sibling === current) continue;
      changed.push([sibling, sibling.inert]);
      sibling.inert = true;
    }
  }
  return () => changed.reverse().forEach(([element, inert]) => { element.inert = inert; });
}

export function useModalDialog({ open = true, busy = false, onClose, focusKey = undefined, preserveOpener = false }) {
  const dialogRef = useRef(null);
  const openerRef = useRef(null);
  const wasOpenRef = useRef(false);
  const viewRef = useRef(focusKey);
  const stateRef = useRef({ busy, onClose });
  stateRef.current = { busy, onClose };
  if (open && !wasOpenRef.current && (!preserveOpener || !openerRef.current)) openerRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  wasOpenRef.current = open;

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!open || !dialog) return undefined;
    const viewChanged = viewRef.current !== focusKey;
    viewRef.current = focusKey;

    const previousFocus = openerRef.current;
    const restoreBackground = inertBackground(dialog);
    const focusFrame = window.requestAnimationFrame(() => {
      if (!viewChanged && dialog.contains(document.activeElement)) return;
      const initial = dialog.querySelector("[autofocus]") || focusableElements(dialog)[0] || dialog;
      initial.focus({ preventScroll: true });
    });

    function handleKeyDown(event) {
      if (event.key === "Escape") {
        if (stateRef.current.busy) return;
        event.preventDefault();
        event.stopPropagation();
        stateRef.current.onClose();
        return;
      }
      if (event.key !== "Tab") return;
      event.stopPropagation();
      const focusable = focusableElements(dialog);
      if (!focusable.length) {
        event.preventDefault();
        dialog.focus();
        return;
      }
      const first = focusable[0];
      const last = focusable.at(-1);
      if (event.shiftKey && (document.activeElement === first || !dialog.contains(document.activeElement))) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    }

    dialog.addEventListener("keydown", handleKeyDown);
    return () => {
      window.cancelAnimationFrame(focusFrame);
      dialog.removeEventListener("keydown", handleKeyDown);
      restoreBackground();
      window.requestAnimationFrame(() => {
        if (!dialog.isConnected && previousFocus?.isConnected) previousFocus.focus({ preventScroll: true });
      });
    };
  }, [open, focusKey]);

  return dialogRef;
}
