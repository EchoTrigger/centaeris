import { Separator } from "@centaeris/frontend-foundations/panels";

export function WorkspaceSplitHandle({ disabled = false, onReset }: {
  disabled?: boolean;
  onReset: () => void;
}) {
  return <Separator className="desktopWorkspaceSeparator" aria-label="Resize workspace panel"
    disabled={disabled} hidden={disabled} disableDoubleClick onDoubleClick={onReset}
    onKeyDown={event => {
      // The library handles arrows, Home and End; retain Enter as an equal-split shortcut.
      if (event.key === "Enter") { event.preventDefault(); onReset(); }
    }} />;
}
