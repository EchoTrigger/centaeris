import { useEffect, useState, StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { AnimatedDisclosure } from "../../src/chat/AnimatedDisclosure";
import { WorkProgress } from "../../src/chat/WorkProgress";
import { WorkspaceContextPanel } from "../../src/components/WorkspaceContextPanel";
import { i18n } from "../../src/i18n";

await i18n.changeLanguage(new URLSearchParams(location.search).get("locale") || "en-US");
const typographyFixture = document.createElement("div");
typographyFixture.className = "workspaceTranscriptBlocks";
typographyFixture.innerHTML = `
  <div class="workProgressStatus" id="workingStatus">工作中 4分39秒</div>
  <div class="workspaceLiveStatus"><span id="liveStatusText">Working</span><span class="workspaceLiveStatusElapsed" id="liveElapsed">4m39s</span></div>
  <button class="agent-operation-summary" id="operationSummary"><span class="agent-tool-node-action" id="operationAction">读取文件 / Read file</span></button>
  <div class="agent-operation-body agent-tool-node-body"><div class="agent-tool-command-card">
    <pre class="agent-tool-bash-command" id="toolCommand"><code id="toolCommandCode">$ cat 文件.txt</code></pre>
    <pre class="agent-tool-output-block" id="toolOutput"><code id="toolOutputCode">工具结果 / Tool output</code></pre>
    <pre class="agent-tool-output-block is-diff" id="toolDiff">+ 新增一行 / Added line</pre>
  </div></div>
  <pre class="workspaceToolOutputScroll" id="toolScroll">Tool output</pre>
  <div class="workspaceToolOutputStatus" id="toolStatus">Complete</div>
  <div class="workspaceUserMessageMeta"><time id="userTime" datetime="2026-10-04T12:00:00Z">20:00</time><button id="userCopy" type="button" aria-label="Copy"><svg id="userCopyIcon" viewBox="0 0 24 24" aria-hidden="true"></svg></button></div>
  <div class="workspaceAssistantMessageMeta"><time id="answerTime" datetime="2026-10-04T12:00:01Z">20:00</time><button id="answerCopy" type="button" aria-label="Copy"><svg id="answerCopyIcon" viewBox="0 0 24 24" aria-hidden="true"></svg></button></div>
`;
document.body.append(typographyFixture);
document.getElementById("code")!.innerHTML = '<code id="codeText">const 正文 = "answer";</code>';
document.getElementById("stage")!.innerHTML = '<p id="stageText">阶段总结 / Stage summary <code id="stageInlineCode">status</code></p><pre id="stagePre"><code id="stageCode">Stage details</code></pre>';
await document.fonts.ready;

const results: { name: string; actual: unknown; expected: unknown }[] = [];
window.addEventListener("error", event => results.push({ name: "uncaught browser error", actual: event.message, expected: "no error" }));
window.addEventListener("unhandledrejection", event => results.push({ name: "unhandled browser rejection", actual: String(event.reason), expected: "no rejection" }));
for (const theme of ["light", "dark"]) {
  document.documentElement.dataset.theme = theme;
  await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
  results.push({ name: `${theme}: body theme is active`, actual: getComputedStyle(document.getElementById("answer")!).color, expected: theme === "dark" ? "rgb(237, 237, 237)" : "rgb(32, 36, 40)" });
  for (const [id, size] of Object.entries({ user: 14, answer: 14, thought: 14, thoughtLink: 14, code: 14, codeText: 14, thoughtCode: 14, tool: 14, work: 14, workingStatus: 14, liveStatusText: 14, liveElapsed: 14, summary: 14, operationSummary: 14, operationAction: 14, toolCommand: 14, toolCommandCode: 14, toolOutput: 14, toolOutputCode: 14, toolDiff: 14, toolScroll: 14, toolStatus: 14, source: 12, stage: 14, stageText: 14, stageInlineCode: 14, stagePre: 14, stageCode: 14, userTime: 12, userCopy: 12, answerTime: 12, answerCopy: 12, document: 14 })) {
    results.push({ name: `${theme}: ${id} font size`, actual: getComputedStyle(document.getElementById(id)!).fontSize, expected: `${size}px` });
  }
  for (const [id, leading] of Object.entries({ thought: 24, tool: 24, thoughtCode: 24, toolOutput: 24, stageText: 24 })) {
    results.push({ name: `${theme}: ${id} line height`, actual: getComputedStyle(document.getElementById(id)!).lineHeight, expected: `${leading}px` });
  }
  for (const id of ["stage", "stageText"]) {
    results.push({ name: `${theme}: ${id} uses body brightness`, actual: getComputedStyle(document.getElementById(id)!).color, expected: getComputedStyle(document.getElementById("answer")!).color });
  }
  for (const id of ["userCopyIcon", "answerCopyIcon"]) {
    const icon = document.getElementById(id)!;
    results.push({ name: `${theme}: ${id} remains auxiliary`, actual: [getComputedStyle(icon).width, getComputedStyle(icon).height], expected: ["12px", "12px"] });
  }
  const error = document.createElement("div");
  error.className = "filePreviewState isError";
  const reference = document.createElement("span");
  reference.style.color = "var(--danger)";
  document.body.append(error, reference);
  results.push({ name: `${theme}: error remains distinguished`, actual: getComputedStyle(error).color, expected: getComputedStyle(reference).color });
  error.remove(); reference.remove();
}
document.documentElement.dataset.theme = "light";

function check(name: string, actual: unknown, expected: unknown) { results.push({ name, actual, expected }); }
const host = document.createElement("div");
document.body.append(host);
const root = createRoot(host);
const render = (node: React.ReactNode) => flushSync(() => root.render(<StrictMode>{node}</StrictMode>));
const settle = () => new Promise(resolve => setTimeout(resolve, 50));
async function finishAnimations() {
  for (const animation of host.getAnimations({ subtree: true })) animation.finish();
  await settle();
}
let mounted = 0;
function Detail() {
  useEffect(() => { mounted++; return () => { mounted--; }; }, []);
  return <p style={{ height: 100 }}>Tool output</p>;
}
try {
  render(<AnimatedDisclosure expanded={false}><Detail /></AnimatedDisclosure>);
  check("closed content is lazy", mounted, 0);
  render(<AnimatedDisclosure expanded><Detail /></AnimatedDisclosure>);
  check("opening mounts content", mounted, 1);
  const opening = host.getAnimations()[0] ?? host.getAnimations({ subtree: true })[0];
  check("opening animates", Boolean(opening), !matchMedia("(prefers-reduced-motion: reduce)").matches);
  await finishAnimations();
  render(<AnimatedDisclosure expanded={false}><Detail /></AnimatedDisclosure>);
  check("closing is immediately inert", host.querySelector(".chatDisclosure")?.hasAttribute("inert"), true);
  const closing = host.getAnimations({ subtree: true })[0];
  if (closing) {
    check("closing retains content during animation", mounted, 1);
    closing.currentTime = 80;
    const height = host.firstElementChild!.getBoundingClientRect().height;
    render(<AnimatedDisclosure expanded><Detail /></AnimatedDisclosure>);
    check("reversal preserves current height", Math.abs(host.firstElementChild!.getBoundingClientRect().height - height) < 2, true);
    await finishAnimations();
    check("stale close does not remove reopened content", mounted, 1);
    render(<AnimatedDisclosure expanded={false}><Detail /></AnimatedDisclosure>);
  }
  await finishAnimations();
  check("closed content releases effects", mounted, 0);
  check("closed disclosure is hidden", (host.firstElementChild as HTMLElement).hidden, true);
  render(<AnimatedDisclosure key="history" expanded><Detail /></AnimatedDisclosure>);
  check("history does not replay entry animation", host.getAnimations({ subtree: true }).length, 0);

  const progress = (finalStarted: boolean, key = "turn") => <WorkProgress key={key} running={!finalStarted} finalStarted={finalStarted}><p>Process</p></WorkProgress>;
  render(progress(false));
  check("running WorkProgress uses body type", getComputedStyle(host.querySelector(".workProgressStatus")!).fontSize, "14px");
  render(progress(true));
  check("completed WorkProgress uses body type", getComputedStyle(host.querySelector(".workProgressSummary span")!).fontSize, "14px");
  await finishAnimations();
  check("final answer folds process", host.querySelector("button")?.getAttribute("aria-expanded"), "false");
  flushSync(() => (host.querySelector("button") as HTMLButtonElement).click());
  render(progress(false));
  render(progress(true));
  check("reopened process survives refresh", host.querySelector("button")?.getAttribute("aria-expanded"), "true");
  render(progress(true, "other-turn"));
  check("another turn uses its own default", host.querySelector("button")?.getAttribute("aria-expanded"), "false");
  function ReadingDetail() {
    const [open, setOpen] = useState(false);
    return <button aria-expanded={open} onClick={() => setOpen(!open)}>Thoughts</button>;
  }
  const reading = (finalStarted: boolean) => <WorkProgress key="reading" running={!finalStarted} finalStarted={finalStarted}><ReadingDetail /></WorkProgress>;
  render(reading(false));
  flushSync(() => (host.querySelector("button") as HTMLButtonElement).click());
  render(reading(true));
  check("final answer preserves actively opened details", host.querySelector(".workProgressSummary")?.getAttribute("aria-expanded"), "true");

  const preview = (open: boolean) => <div className={`workspaceWorkbench${open ? " withContextPanel withFilePreview" : ""}`}><div className="workspaceChatColumn" /><WorkspaceContextPanel
    panel={open ? { mode: "filePreview", status: "ready", displayName: "Evidence.txt", preview: { kind: "text", content: "Evidence", contentType: "text/plain" } } : { mode: "closed" }}
    browserWidthPx={480} onBrowserWidthChange={() => {}} onClose={() => {}} onReturn={() => {}} /></div>;
  render(preview(false));
  render(preview(true));
  check("preview ready content visible", host.querySelector(".documentTextPreview")?.textContent, "Evidence");
  render(preview(false));
  check("preview close releases file content", host.querySelector(".documentTextPreview"), null);
  check("preview exit shell is inert", host.querySelector("aside")?.hasAttribute("inert"), true);
  if (matchMedia("(prefers-reduced-motion: reduce)").matches) {
    check("reduced motion removes exit delay", getComputedStyle(host.querySelector("aside")!).transitionDelay, "0s");
  }
} catch (error) {
  results.push({ name: "browser exception", actual: String(error), expected: "no exception" });
}
root.unmount();
await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
check("all chat presentation checks completed", true, true);
document.getElementById("results")!.textContent = JSON.stringify(results);
