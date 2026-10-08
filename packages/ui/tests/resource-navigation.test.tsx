import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { expect, test, vi } from "vitest";
import { PluginsDialog } from "../src/components/PluginsDialog";
import { useState } from "react";
import { useAppNavigation, type AppLocation } from "../src/components/app/useAppNavigation";
import { SkillsDialog } from "../src/components/SkillsDialog";
import { getPluginDetail, setPluginEnabled, reloadPlugins } from "../src/lib/chatBridge";
vi.mock("../src/lib/chatBridge", () => ({
 listPlugins: vi.fn(async () => [{ id:"p",name:"Example",description:"Plugin description",source:"managed",enabled:true,errors:[],tools:[],path:"/p" }]),
 getPluginDetail: vi.fn(async () => ({descriptor:{id:"p"},capabilities:{skills:[],cli:[],apps:[],hooks:[],mcpServers:[],capabilities:[]}})),
 setPluginEnabled: vi.fn(async () => ({})),
 reloadPlugins: vi.fn(async () => ({})),
 listSkillSources: vi.fn(async () => ({sources:[],skillPolicies:[]})),
 getSkillCatalog: vi.fn(async () => ({skills:[{skillId:"s",sourceId:"src",scope:"user",name:"Design",description:"Design guidance",enabled:true,errors:[],capabilityMetadata:{allowedTools:[]}}, ...(["workspace", "system", "plugin"] as const).map(scope => ({skillId:scope,sourceId:scope,scope,name:scope,description:"",enabled:true,errors:[],capabilityMetadata:{allowedTools:[]}}))],diagnostics:[]})),
 getSkillDetail: vi.fn(async () => ({skill:{skillId:"s",scope:"user",name:"Design",description:"Design guidance",enabled:true,errors:[],capabilityMetadata:{allowedTools:[]}},content:"Instructions"})),
}));
vi.mock("../src/components/chat/MarkdownContent", () => ({MarkdownContent: ({text}:{text:string}) => <p>{text}</p>}));
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
test("plugin selection navigates to detail while list search survives return", async () => {
 let view!:ReactTestRenderer;
 await act(async () => { view=create(<PluginsDialog />); });
 await act(async () => view.root.findByProps({"aria-label":"Search plugins"}).props.onChange({target:{value:"Example"}}));
 await act(async () => view.root.findAllByType("button").find(b => b.props["aria-label"] === "Open Example")!.props.onClick());
 expect(view.root.findByProps({"aria-label":"Plugin navigation"})).toBeTruthy();
 await act(async () => view.root.findByProps({"aria-label":"Back to plugins"}).props.onClick());
 expect(view.root.findByProps({"aria-label":"Search plugins"}).props.value).toBe("Example");
 await act(async () => view.unmount());
});

test("plugin detail discarded after returning to the list cannot surface a late error", async () => {
 let reject!:(error:Error)=>void;
 vi.mocked(getPluginDetail).mockImplementationOnce(()=>new Promise((_resolve,fail)=>{reject=fail;}));
 let view!:ReactTestRenderer;
 await act(async()=>{view=create(<PluginsDialog/>);});
 await act(async()=>view.root.findByProps({"aria-label":"Open Example"}).props.onClick());
 await act(async()=>view.root.findByProps({"aria-label":"Back to plugins"}).props.onClick());
 await act(async()=>reject(new Error("obsolete detail error")));
 expect(JSON.stringify(view.toJSON())).not.toContain("obsolete detail error");
 expect(view.root.findByProps({"aria-label":"Search plugins"})).toBeTruthy();
 await act(async()=>view.unmount());
});

test("plugin reload and enablement controls invoke the selected plugin operations", async () => {
 vi.mocked(reloadPlugins).mockClear(); vi.mocked(setPluginEnabled).mockClear();
 let view!:ReactTestRenderer;
 await act(async()=>{view=create(<PluginsDialog/>);});
 await act(async()=>view.root.findByProps({"aria-label":"Reload plugins"}).props.onClick());
 expect(reloadPlugins).toHaveBeenCalledExactlyOnceWith();
 await act(async()=>view.root.findByProps({"aria-label":"Open Example"}).props.onClick());
 await act(async()=>view.root.findByProps({"aria-label":"Disable plugin"}).props.onClick());
 expect(setPluginEnabled).toHaveBeenCalledExactlyOnceWith({id:"p",enabled:false});
 await act(async()=>view.unmount());
});
test("skill opens in a dismissible dialog and preserves list query", async () => {
 let view!:ReactTestRenderer;
 await act(async () => {view=create(<SkillsDialog confirmAction={async()=>true}/>);});
 await act(async () => view.root.findByProps({"aria-label":"Search skills"}).props.onChange({target:{value:"Design"}}));
 await act(async () => view.root.findByProps({"aria-label":"Open Design"}).props.onClick());
 expect(view.root.findByType("dialog").props["aria-label"]).toBe("Design");
 await act(async () => view.root.findByProps({"aria-label":"Close detail"}).props.onClick());
 expect(view.root.findAllByType("dialog")).toHaveLength(0);
 expect(view.root.findByProps({"aria-label":"Search skills"}).props.value).toBe("Design");
 await act(async () => view.unmount());
});

test("global back and forward restore the plugin detail with the list query intact", async () => {
 let nav!:ReturnType<typeof useAppNavigation>;
 function Harness() {
  const [location,setLocation] = useState<AppLocation>({page:"plugins",sessionId:null,root:null,pluginId:""});
  nav=useAppNavigation(location,async next => {setLocation(next);return true;});
  return <PluginsDialog selectedPluginId={location.pluginId} onSelect={pluginId => {void nav.move({...location,pluginId});}}/>;
 }
 let view!:ReactTestRenderer;
 await act(async()=>{view=create(<Harness/>);});
 await act(async()=>view.root.findByProps({"aria-label":"Search plugins"}).props.onChange({target:{value:"Example"}}));
 await act(async()=>view.root.findByProps({"aria-label":"Open Example"}).props.onClick());
 await act(async()=>nav.move(-1));
 expect(view.root.findAllByProps({"aria-label":"Plugin navigation"})).toHaveLength(0);
 expect(view.root.findByProps({"aria-label":"Search plugins"}).props.value).toBe("Example");
 await act(async()=>nav.move(1));
 expect(view.root.findByProps({"aria-label":"Plugin navigation"})).toBeTruthy();
 await act(async()=>view.unmount());
});

test("skill scope filters the catalog and Escape closes detail without resetting search", async () => {
 let view!:ReactTestRenderer;
 await act(async()=>{view=create(<SkillsDialog confirmAction={async()=>true}/>);});
 await act(async()=>view.root.findAllByType("button").find(b=>b.children.includes("System"))!.props.onClick());
 expect(view.root.findAllByProps({"aria-label":"Open Design"})).toHaveLength(0);
 await act(async()=>view.root.findAllByType("button").find(b=>b.children.includes("Personal"))!.props.onClick());
 await act(async()=>view.root.findByProps({"aria-label":"Search skills"}).props.onChange({target:{value:"Design"}}));
 await act(async()=>view.root.findByProps({"aria-label":"Open Design"}).props.onClick());
 const preventDefault=vi.fn();
 await act(async()=>view.root.findByType("dialog").props.onCancel({preventDefault}));
 expect(preventDefault).toHaveBeenCalled();
 expect(view.root.findAllByType("dialog")).toHaveLength(0);
 expect(view.root.findByProps({"aria-label":"Search skills"}).props.value).toBe("Design");
 await act(async()=>view.unmount());
});

test("Skill has only Personal and System; Personal includes workspace skills but never plugin skills", async () => {
 let view!:ReactTestRenderer;
 await act(async()=>{view=create(<SkillsDialog workspaceRoot="/project" confirmAction={async()=>true}/>);});
 const filters=view.root.findByProps({"aria-label":"Skill scope"}).findAllByType("button");
 expect(filters.map(button=>button.children.join(""))).toEqual(["Personal","System"]);
 expect(filters[0].props["aria-pressed"]).toBe(true);
 expect(view.root.findByProps({"aria-label":"Open Design"})).toBeTruthy();
 expect(view.root.findByProps({"aria-label":"Open workspace"})).toBeTruthy();
 expect(view.root.findAllByProps({"aria-label":"Open system"})).toHaveLength(0);
 expect(view.root.findAllByProps({"aria-label":"Open plugin"})).toHaveLength(0);
 await act(async()=>filters[1].props.onClick());
 expect(view.root.findByProps({"aria-label":"Open system"})).toBeTruthy();
 for(const name of ["Design","workspace","plugin"]) expect(view.root.findAllByProps({"aria-label":`Open ${name}`})).toHaveLength(0);
 await act(async()=>view.unmount());
});
