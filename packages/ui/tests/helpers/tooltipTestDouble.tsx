import type { ReactNode } from "react";
// Domain/controller suites isolate floating layout. Real focus and dismissal
// behavior is covered by frontend-primitives.test.tsx with the upstream popup.
export const Tooltip = ({ children }: { children: ReactNode }) => children;
export const TooltipTrigger = ({ render }: { render: ReactNode }) => render;
export const TooltipContent = () => null;
export const TooltipProvider = Tooltip;
