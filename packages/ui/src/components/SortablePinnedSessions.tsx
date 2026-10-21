import { DragDropProvider } from "@dnd-kit/react";
import { isSortable, useSortable } from "@dnd-kit/react/sortable";
import { GripVertical } from "lucide-react";
import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import type { UiSession } from "../types/ui";
import "../styles/desktop-layout.css";

function SortableSession({ session, index, disabled, descriptionId, renderSession }: {
  session: UiSession;
  index: number;
  disabled: boolean;
  descriptionId: string;
  renderSession: (session: UiSession, handle?: ReactNode) => ReactNode;
}) {
  const { ref, handleRef, isDragSource } = useSortable({
    id: session.id, index, group: "pinned-sessions", type: "pinned-session",
    accept: "pinned-session", disabled,
  });
  return <div ref={ref} className="pinnedSortableRow" data-dragging={isDragSource || undefined}>
    {renderSession(session, <button ref={handleRef} type="button" className="pinnedDragHandle"
      aria-label={`Reorder ${session.title}`} aria-describedby={descriptionId}
      aria-roledescription="sortable session" disabled={disabled}>
      <GripVertical aria-hidden="true" />
    </button>)}
  </div>;
}

export function SortablePinnedSessions({ sessions, disabled, onReorder, renderSession }: {
  sessions: UiSession[];
  disabled: boolean;
  onReorder: (ids: string[]) => Promise<void>;
  renderSession: (session: UiSession, handle?: ReactNode) => ReactNode;
}) {
  const [saving, setSaving] = useState(false);
  const [dragging, setDragging] = useState(false);
  const [visibleSessions, setVisibleSessions] = useState(sessions);
  useEffect(() => {
    if (!dragging && !saving) setVisibleSessions(sessions);
  }, [sessions, dragging, saving]);
  const pending = useRef(false);
  const startOrder = useRef<string[]>([]);
  const descriptionId = useId();
  const ids = sessions.map(session => session.id);
  const unavailable = disabled || saving;
  return <DragDropProvider
    onBeforeDragStart={event => {
      if (unavailable || pending.current) { event.preventDefault(); return; }
      startOrder.current = visibleSessions.map(session => session.id);
    }}
    onDragStart={() => setDragging(true)}
    onDragEnd={event => {
      setDragging(false);
      const { source, target } = event.operation;
      if (event.canceled || !target || unavailable || pending.current || !isSortable(source)) return;
      const initial = startOrder.current;
      // A catalog refresh may have changed membership or order while dragging.
      // Never persist a stale, partial list over that authoritative update.
      if (initial.length !== ids.length || initial.some((id, index) => id !== ids[index])) return;
      const { initialIndex, index } = source;
      if (initialIndex === index || initialIndex < 0 || index < 0 || index >= initial.length) return;
      const order = [...initial];
      const [moved] = order.splice(initialIndex, 1);
      if (moved !== source.id) return;
      order.splice(index, 0, moved);
      const byId = new Map(visibleSessions.map(session => [session.id, session]));
      setVisibleSessions(order.map(id => byId.get(id)!));
      pending.current = true;
      setSaving(true);
      void onReorder(order).catch(() => {
        // The controller reports the error and reconciles an uncertain write.
      }).finally(() => { pending.current = false; setSaving(false); });
    }}>
    <span id={descriptionId} className="desktopVisuallyHidden">Press Space to pick up a session, use the arrow keys to move it, then Space to save. Press Escape to cancel.</span>
    {visibleSessions.map((session, index) => <SortableSession key={session.id} session={session} index={index}
      disabled={unavailable} descriptionId={descriptionId} renderSession={renderSession} />)}
  </DragDropProvider>;
}
