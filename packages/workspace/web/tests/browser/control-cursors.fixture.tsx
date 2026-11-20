import { createRoot } from "react-dom/client";
import { FrontendProvider } from "@centaeris/ui/provider";
import { Input } from "@centaeris/ui/input";
import { Textarea } from "@centaeris/ui/textarea";
import { Checkbox } from "@centaeris/ui/checkbox";
import { Switch } from "@centaeris/ui/switch";
import { Select, SelectTrigger, SelectValue } from "@centaeris/ui/select";
import "../../src/globals.css";

createRoot(document.getElementById("fixture")!).render(<FrontendProvider>
  <main>
    <button data-cursor-sample="web-global" disabled>Unavailable action</button>
    <div className="librarySelectionBar"><button data-cursor-sample="library-selection" disabled>Select</button></div>
    <div className="libraryDialogActions"><button data-cursor-sample="library-dialog" disabled>Confirm</button></div>
    <div className="libraryMoveActions"><button data-cursor-sample="library-move" disabled>Move</button></div>
    <Input data-cursor-sample="shared-input" aria-label="Input" disabled />
    <Textarea data-cursor-sample="shared-textarea" aria-label="Textarea" disabled />
    <Checkbox data-cursor-sample="shared-checkbox" aria-label="Checkbox" disabled />
    <Switch data-cursor-sample="shared-switch" aria-label="Switch" disabled />
    <Select disabled><SelectTrigger data-cursor-sample="shared-select" aria-label="Select"><SelectValue /></SelectTrigger></Select>
  </main>
</FrontendProvider>);
