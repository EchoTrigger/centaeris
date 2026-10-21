import type { ReactNode } from "react";
import { MoreHorizontal } from "lucide-react";
import { Button } from "./ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuTrigger } from "./ui/dropdown-menu";

export function ChatActionsMenu({ children, label = "Chat actions" }: { children: ReactNode; label?: string }) {
  return <DropdownMenu>
    <DropdownMenuTrigger render={<Button variant="ghost" size="icon" aria-label={label} />}><MoreHorizontal size={18} aria-hidden="true" /></DropdownMenuTrigger>
    <DropdownMenuContent align="end" className="chatActionsMenuContent">{children}</DropdownMenuContent>
  </DropdownMenu>;
}
