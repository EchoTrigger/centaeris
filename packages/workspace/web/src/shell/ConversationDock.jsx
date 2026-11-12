import { createContext, useContext } from "react";
import { createPortal } from "react-dom";

export const ConversationDockContext = createContext(null);

export function ConversationDockItem({ children }) {
  const dock = useContext(ConversationDockContext);
  return dock ? createPortal(children, dock) : children;
}
