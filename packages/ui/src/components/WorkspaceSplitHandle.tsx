import { useRef, useState, type PointerEvent } from "react";

export function WorkspaceSplitHandle({
  value,
  onChange,
}: {
  value: number;
  onChange: (value: number) => void;
}) {
  const pointer = useRef<number | null>(null);
  const [dragging, setDragging] = useState(false);
  const update = (next: number, element: HTMLDivElement) => {
    const width = element.parentElement?.getBoundingClientRect().width ?? 0;
    const minimum = width > 0 ? Math.min(0.5, Math.max(0.25, 320 / width)) : 0.25;
    onChange(Math.max(minimum, Math.min(1 - minimum, next)));
  };
  const finish = (event: PointerEvent<HTMLDivElement>) => {
    if (pointer.current !== event.pointerId) return;
    pointer.current = null;
    setDragging(false);
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
  };
  return (
    <div
      role="separator"
      tabIndex={0}
      aria-label="Resize workspace panel"
      aria-orientation="vertical"
      aria-controls="workspace-preview"
      aria-valuemin={25}
      aria-valuemax={75}
      aria-valuenow={Math.round(value * 100)}
      aria-valuetext={`Workspace panel ${Math.round(value * 100)} percent`}
      className={`workspaceSplitHandle ${dragging ? "is-dragging" : ""}`}
      onPointerDown={(event) => {
        if (event.button !== 0) return;
        event.preventDefault();
        event.currentTarget.setPointerCapture(event.pointerId);
        pointer.current = event.pointerId;
        setDragging(true);
      }}
      onPointerMove={(event) => {
        if (pointer.current !== event.pointerId) return;
        const bounds = event.currentTarget.parentElement?.getBoundingClientRect();
        if (bounds && bounds.width > 0)
          update((bounds.left + bounds.width - event.clientX) / bounds.width, event.currentTarget);
      }}
      onPointerUp={finish}
      onPointerCancel={finish}
      onLostPointerCapture={() => {
        pointer.current = null;
        setDragging(false);
      }}
      onDoubleClick={() => onChange(0.5)}
      onKeyDown={(event) => {
        const next =
          event.key === "ArrowLeft"
            ? value + 0.02
            : event.key === "ArrowRight"
              ? value - 0.02
              : event.key === "Home"
                ? 0.25
                : event.key === "End"
                  ? 0.75
                  : event.key === "Enter"
                    ? 0.5
                    : null;
        if (next === null) return;
        event.preventDefault();
        update(next, event.currentTarget);
      }}
    />
  );
}
