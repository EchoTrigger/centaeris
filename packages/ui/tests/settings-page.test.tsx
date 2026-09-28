import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, test, vi } from "vitest";
import { SettingsPage } from "../src/components/SettingsPage";
import { changeTheme } from "../src/theme";
vi.mock("../src/theme", () => ({ useThemePreference: () => "system", changeTheme: vi.fn() }));
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
test("settings expose theme choices and model services without a duplicate Settings heading", async () => {
 let view!: ReactTestRenderer;
 await act(async () => { view = create(<SettingsPage><p>Model content</p></SettingsPage>); });
 expect(view.root.findAllByType("h1").map(x=>x.children.join(""))).toEqual(["Appearance"]);
 expect(view.root.findAllByType("option").map(x=>x.props.value)).toEqual(["system", "light", "dark"]);
 await act(async()=>view.root.findByType("select").props.onChange({target:{value:"dark"}}));
 expect(changeTheme).toHaveBeenCalledWith("dark");
 await act(async()=>view.root.findAllByType("button")[1].props.onClick());
 expect(JSON.stringify(view.toJSON())).toContain("Model content");
 await act(async()=>view.unmount());
});
