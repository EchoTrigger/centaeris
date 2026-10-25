import type { ComposerPlaneProps } from "../../src/components/chat/ComposerPlane";

// Stream/model tests use partial window mocks; DOM layout is covered by welcome-draft.
export function ComposerPlane({ welcome, children }: ComposerPlaneProps) {
  return <div role="region" aria-label="Message composer" data-position={welcome ? "center" : "bottom"} className={`chatBottomPlane ${welcome ? "is-welcome" : ""}`}>{children}</div>;
}
