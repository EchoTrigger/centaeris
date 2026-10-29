import { createRoot } from "react-dom/client";
import { FrontendProvider } from "@centaeris/frontend-foundations/provider";
import { Input } from "@centaeris/frontend-foundations/input";
import { Textarea } from "@centaeris/frontend-foundations/textarea";
import { Checkbox } from "@centaeris/frontend-foundations/checkbox";
import { Switch } from "@centaeris/frontend-foundations/switch";
import { Select, SelectTrigger, SelectValue } from "@centaeris/frontend-foundations/select";
import "../../../ui/src/index.css";
import "../../src/globals.css";

createRoot(document.getElementById("fixture")!).render(<FrontendProvider>
  <main>
    <button data-cursor-sample="web-global" disabled>Unavailable action</button>
    <div className="librarySelectionBar"><button data-cursor-sample="library-selection" disabled>Select</button></div>
    <div className="libraryDialogActions"><button data-cursor-sample="library-dialog" disabled>Confirm</button></div>
    <div className="libraryMoveActions"><button data-cursor-sample="library-move" disabled>Move</button></div>
    <button data-cursor-sample="desktop-panel" className="nativePanelToggle" disabled>Panel</button>
    <div className="thinSidebarFooter"><button data-cursor-sample="desktop-sidebar" disabled>Sidebar</button></div>
    <button data-cursor-sample="desktop-edit" className="user-edit-primary" disabled>Edit</button>
    <button data-cursor-sample="desktop-request" className="pending-request-btn" disabled>Request</button>
    <button data-cursor-sample="desktop-send" className="send-button" disabled>Send</button>
    <button data-cursor-sample="desktop-settings" className="runtime-settings-save" disabled>Save</button>
    <Input data-cursor-sample="shared-input" aria-label="Input" disabled />
    <Textarea data-cursor-sample="shared-textarea" aria-label="Textarea" disabled />
    <Checkbox data-cursor-sample="shared-checkbox" aria-label="Checkbox" disabled />
    <Switch data-cursor-sample="shared-switch" aria-label="Switch" disabled />
    <Select disabled><SelectTrigger data-cursor-sample="shared-select" aria-label="Select"><SelectValue /></SelectTrigger></Select>
  </main>
</FrontendProvider>);
