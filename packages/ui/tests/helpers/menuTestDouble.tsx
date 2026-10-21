import { isValidElement, type ComponentProps, type ReactNode } from "react";
// App/controller unit suites isolate popup layout. Actual keyboard/focus behavior
// is exercised with the real Base UI menu in chat-actions-menu.test.tsx.
export const DropdownMenu = ({ children }: { children: ReactNode }) => children;
export const DropdownMenuContent = DropdownMenu;
export const DropdownMenuTrigger = ({ render, children }: { render: ReactNode; children: ReactNode }) =>
  isValidElement<ComponentProps<"button">>(render) ? <button {...render.props}>{children}</button> : null;
export const DropdownMenuItem = ({ children, ...props }: ComponentProps<"button">) => <button {...props}>{children}</button>;
