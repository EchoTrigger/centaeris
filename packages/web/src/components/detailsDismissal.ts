export function dismissDetailsOnOutsideInteraction(details: HTMLDetailsElement) {
  const document = details.ownerDocument;
  const closeOutside = (event: Event) => {
    if (details.open && !details.contains(event.target as Node)) details.open = false;
  };
  const closeOnEscape = (event: KeyboardEvent) => {
    if (!details.open || event.key !== "Escape") return;
    event.preventDefault();
    details.open = false;
    details.querySelector("summary")?.focus();
  };
  document.addEventListener("pointerdown", closeOutside);
  document.addEventListener("focusin", closeOutside);
  document.addEventListener("keydown", closeOnEscape);
  return () => {
    document.removeEventListener("pointerdown", closeOutside);
    document.removeEventListener("focusin", closeOutside);
    document.removeEventListener("keydown", closeOnEscape);
  };
}
