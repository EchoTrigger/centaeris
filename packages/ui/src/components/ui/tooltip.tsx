import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

type TooltipProps = {
  align?: "center" | "end" | "start";
  children: ReactNode;
  className?: string;
  content?: ReactNode;
  side?: "top" | "bottom";
};

function Tooltip({
  align = "center",
  children,
  className,
  content,
  side = "top",
}: TooltipProps) {
  const hasContent = Boolean(content);
  return (
    <span
      className={cn(
        "uiTooltip",
        `uiTooltip-${side}`,
        `uiTooltip-align-${align}`,
        className,
      )}
    >
      {children}
      {hasContent ? (
        <span className="uiTooltipBubble" role="tooltip">
          {content}
        </span>
      ) : null}
    </span>
  );
}

export { Tooltip };
