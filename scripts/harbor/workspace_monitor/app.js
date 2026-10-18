"use strict";
// Event text is always inserted as text nodes, never HTML or executable URLs.
const byId = (id) => document.getElementById(id);
let selected = null;
let page = 0;
let current = null;
const pageSize = 20;
const slotViews = new Map();
let connected = false;
function node(tag, value, className) {
  const element = document.createElement(tag);
  if (value !== undefined) element.textContent = String(value);
  if (className) element.className = className;
  return element;
}
function display(value) { return value === null || value === undefined ? "Not observed" : String(value); }
function fact(label, value) { byId("facts").append(node("dt", label), node("dd", display(value))); }
function metric(value) { return value === null || value === undefined ? "Unknown" : String(value); }
function bytes(value) {
  if (value === null || value === undefined) return "Unknown";
  if (value < 1024) return `${value} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let scaled = value / 1024;
  let unit = 0;
  while (scaled >= 1024 && unit < units.length - 1) { scaled /= 1024; unit++; }
  return `${scaled.toFixed(2)} ${units[unit]}`;
}
function metricFact(target, label, value) { byId(target).append(node("dt", label), node("dd", value)); }
function renderMetrics(row) {
  byId("metrics").hidden = !row;
  byId("usage-facts").replaceChildren();
  byId("memory-facts").replaceChildren();
  const sample = row && row.metrics;
  byId("metrics-content").hidden = !sample;
  byId("metrics-note").textContent = sample
    ? `Observed at ${sample.observedAt}. Committed main-agent usage excludes in-flight requests and context capacity. Runtime memory is shared by all trials; do not sum it across trials.`
    : "Metrics not observed.";
  if (!sample) return;
  const usage = sample.usage;
  metricFact("usage-facts", "Committed reports", metric(usage.requests));
  metricFact("usage-facts", "Input / output tokens", `${metric(usage.inputTokens)} / ${metric(usage.outputTokens)}`);
  metricFact("usage-facts", "Cache hit / miss tokens", `${metric(usage.promptCacheHitTokens)} / ${metric(usage.promptCacheMissTokens)}`);
  metricFact("usage-facts", "Cache hit rate", usage.promptCacheHitRate === null ? "Unknown" : `${(usage.promptCacheHitRate * 100).toFixed(1)}%`);
  metricFact("usage-facts", "Usage coverage", usage.complete ? "Complete" : "Incomplete / unavailable fields remain unknown");
  const memory = sample.memory;
  metricFact("memory-facts", "Shared Runtime current / peak", `${bytes(memory.runtimeCurrentBytes)} / ${bytes(memory.runtimePeakBytes)}`);
  metricFact("memory-facts", "Shared Runtime anonymous", bytes(memory.runtimeAnonBytes));
  metricFact("memory-facts", "Task current / peak", `${bytes(memory.taskCurrentBytes)} / ${bytes(memory.taskPeakBytes)}`);
  metricFact("memory-facts", "Task limit", bytes(memory.taskLimitBytes));
  metricFact("memory-facts", "OOM kills: shared Runtime / task", `${metric(memory.runtimeOomKills)} / ${metric(memory.taskOomKills)}`);
}
function renderDetails(state, rows) {
  const row = rows.find((item) => item.key === selected);
  byId("facts").replaceChildren();
  byId("selected").textContent = row ? `${row.taskId} / attempt ${row.attempt}` : "Select a task";
  if (row) {
    for (const [label, key] of [["Trial", "trialId"], ["Status", "status"], ["Phase", "phase"], ["Agent", "agentStatus"], ["Verifier", "verifierStatus"], ["Reward", "reward"], ["Session", "sessionId"], ["Agent run", "agentRunId"], ["Environment", "environmentId"], ["Instance", "instanceId"], ["Container", "containerId"], ["Cleanup", "cleanupStatus"], ["Error", "error"]]) fact(label, row[key]);
    fact("Memory", `${row.memoryMiB} MiB${row.memoryMiB >= 8192 ? " / exclusive through cleanup" : ""}`);
    fact("Holding", row.resourceHolding === undefined || row.resourceHolding === null ? null : `${row.resourceHolding} instance debit`);
  }
  renderMetrics(row);
  const logs = state.logs.filter((entry) => !row || !entry.trialId || entry.trialId === row.trialId).slice(-20).reverse();
  byId("logs").replaceChildren();
  for (const entry of logs) {
    const item = node("li");
    const timestamp = node("time", entry.observedAt.slice(11, 19));
    timestamp.dateTime = entry.observedAt;
    item.append(timestamp, node("span", entry.message, entry.level === "error" ? "error" : ""));
    byId("logs").append(item);
  }
  if (!logs.length) byId("logs").append(node("li", "No recent observations.", "muted"));
}
function elapsed(row, live = true) {
  if (!row || !row.startedAt) return 'Unknown';
  const start = Date.parse(row.startedAt);
  const end = row.finishedAt ? Date.parse(row.finishedAt) : live ? Date.now() : Date.parse(row.observedAt);
  if (!Number.isFinite(start) || !Number.isFinite(end)) return 'Unknown';
  // A bounded display format; after 999 hours the label explicitly saturates.
  const seconds = Math.max(0, Math.floor((end - start) / 1000));
  if (seconds >= 1000 * 3600) return '999h+';
  return `${String(Math.floor(seconds / 3600)).padStart(2,'0')}:${String(Math.floor(seconds / 60) % 60).padStart(2,'0')}:${String(seconds % 60).padStart(2,'0')}`;
}
function slotView(slot) {
  if (slotViews.has(slot)) return slotViews.get(slot);
  const section = node('section', undefined, 'slot');
  const heading = node('div', undefined, 'slot-title');
  const timer = node('time', 'Unknown', 'elapsed');
  timer.setAttribute('aria-label', `Slot ${slot} elapsed time`);
  timer.title = 'Time from first observed running event to first observed terminal outcome; cleanup ownership is shown separately.';
  heading.append(node('h2', `Slot ${slot}`), timer);
  const task = node('h3', 'No observed trial');
  const status = node('div', '', 'muted');
  const detail = node('div', '', 'muted');
  const memory = node('div', '', 'muted');
  const toolbar = node('div', undefined, 'output-toolbar');
  const note = node('span', 'Output not observed', 'muted');
  const follow = node('button', 'Following');
  follow.setAttribute('aria-label', `Follow Slot ${slot} output`);
  follow.setAttribute('aria-pressed', 'true');
  toolbar.append(note, follow);
  const log = node('div', undefined, 'output-log');
  log.setAttribute('role', 'log');
  log.setAttribute('aria-label', `Slot ${slot} output`);
  log.setAttribute('aria-live', 'off');
  log.tabIndex = 0;
  const view = {section, timer, task, status, detail, memory, note, follow, log,
    trialId:null, following:true, signature:null, row:null, entries:new Map()};
  function followState(value) {
    view.following = value;
    follow.textContent = value ? 'Following' : 'Follow output';
    follow.setAttribute('aria-pressed', String(value));
  }
  log.addEventListener('scroll', () => followState(log.scrollHeight-log.clientHeight-log.scrollTop<32), {passive:true});
  follow.onclick = () => { followState(true); log.scrollTop=log.scrollHeight; };
  section.append(heading, task, status, detail, memory, toolbar, log);
  byId('slots').append(section);
  slotViews.set(slot, view);
  return view;
}
function renderSlot(state, slot) {
  const view = slotView(slot.slot);
  const assigned = state.trials.filter(t => t.slot===slot.slot);
  const row = state.trials.find(t => slot.trialIds.includes(t.trialId)) || assigned[assigned.length-1];
  const output = row && (state.outputs || []).find(o => o.trialId===row.trialId);
  view.row = row;
  view.task.textContent = row ? row.taskId : 'No observed trial';
  view.status.textContent = state.job && slot.slot>state.job.maxConcurrent ? 'Disabled by job concurrency'
    : slot.trialIds.length ? 'Resource held' : state.exclusiveTrial ? 'Exclusive task holds capacity'
    : row ? 'Last observed trial' : 'No resource holder observed';
  view.detail.textContent = row ? `${row.phase} / Agent: ${display(row.agentStatus)} / Verifier: ${display(row.verifierStatus)}` : 'Waiting for a trial assignment';
  const memory = row && row.metrics && row.metrics.memory;
  view.memory.textContent = memory ? `Task memory: ${bytes(memory.taskCurrentBytes)} / ${bytes(memory.taskLimitBytes)}` : 'Task memory: Unknown';
  view.timer.textContent = elapsed(row, connected && state.monitorStatus==='live');
  view.note.textContent = output ? (output.error ? 'Output unavailable / last observations' : state.outputError ? 'Output observations stale or unavailable' : 'Public Workspace output')
    : state.outputError ? `Output unavailable: ${state.outputError}` : 'Output not observed';
  const signature = JSON.stringify([state.job && state.job.jobId, row && row.trialId, output && output.entries, output && output.truncated]);
  if (signature===view.signature) return;
  view.signature = signature;
  const jobId = state.job && state.job.jobId;
  const changed = view.trialId !== (row && row.trialId) || view.jobId!==jobId;
  if (changed) {
    view.trialId = row && row.trialId;
    view.jobId = jobId;
    view.following = true;
    view.follow.textContent = 'Following';
    view.follow.setAttribute('aria-pressed', 'true');
    view.log.replaceChildren(); view.entries.clear();
  }
  const oldTop = view.log.scrollTop;
  const anchor = [...view.log.children].find(el => el.getBoundingClientRect().bottom>view.log.getBoundingClientRect().top);
  const anchorTop = anchor && anchor.getBoundingClientRect().top;
  const entries = output ? output.entries.slice(-40) : [];
  const keep = new Set(entries.map(e => e.id));
  for (const [id, element] of view.entries) if (!keep.has(id)) { element.remove(); view.entries.delete(id); }
  for (const entry of entries) {
    let article = view.entries.get(entry.id);
    if (!article) {article=node('article');article.append(node('h4'), node('pre'));view.entries.set(entry.id,article);}
    if (article.firstChild.textContent!==entry.kind) article.firstChild.textContent=entry.kind;
    if (article.lastChild.textContent!==entry.text) article.lastChild.textContent=entry.text;
    // Existing nodes retain identity; appending in source order also handles a sealed overlay.
    view.log.append(article);
  }
  let empty = view.log.querySelector('.output-empty');
  if (!entries.length && !empty) {empty=node('p','No output has been observed for this trial.','output-empty muted');view.log.append(empty);}
  if (entries.length && empty) empty.remove();
  let gap = view.log.querySelector('.output-gap');
  if (output && output.truncated && !gap) {gap=node('p','Earlier output is outside this bounded view.','output-gap muted');view.log.prepend(gap);}
  if ((!output || !output.truncated) && gap) gap.remove();
  if (view.following) view.log.scrollTop=view.log.scrollHeight;
  else if (anchor && anchor.isConnected) view.log.scrollTop=oldTop+anchor.getBoundingClientRect().top-anchorTop;
  else view.log.scrollTop=oldTop;
}
function render(state) {
  current = state;
  const job = state.job;
  byId("connection").textContent = `Monitor: ${state.monitorStatus}`;
  byId("title").textContent = job ? job.title : "No live run";
  byId("job-status").textContent = `Job status: ${state.jobStatus}`;
  const messages = [];
  if (state.monitorError) messages.push(`Observation error: ${state.monitorError}. Job outcome remains independent.`);
  if (state.monitorStatus === "stale") messages.push("Observations are stale. Running tasks have not been reclassified as failed.");
  if (state.catchingUp) messages.push("Reading the next bounded event page.");
  if (state.cursorResetCount) messages.push(`Event stream replaced or truncated: projection rebuilt ${state.cursorResetCount} time(s).`);
  byId("warning").textContent = messages.join("\n");
  byId("warning").hidden = !messages.length;
  const model = job && job.model;
  byId("configuration").textContent = job ? `${job.taskCount} distinct tasks / ${job.attemptsPerTask} attempt(s) / at most ${job.maxConcurrent} concurrent${model ? ` / ${model.modelName || "Model not recorded"} / ${model.thinkingMode || "Thinking not recorded"} / ${model.contextTokens || "?"} context tokens` : ""}` : "Waiting for an explicit job manifest and observed events.";
  const finished = state.trials.filter((trial) => ["completed", "failed", "cancelled"].includes(trial.status)).length;
  byId("counts").textContent = job ? `${finished} / ${job.trialCount} terminal outcomes observed` : "No observations";
  byId("progress-label").textContent = job ? `${state.trials.length} trial(s) observed / ${job.trialCount} configured` : "Completion not observed";
  byId("progress").max = job ? job.trialCount : 1;
  byId("progress").value = finished;
  for (const slot of state.slots) {
    renderSlot(state, slot);
  }
  const rows = [];
  if (job) for (const task of job.tasks) for (let attempt = 1; attempt <= job.attemptsPerTask; attempt++) {
    const observed = state.trials.find((trial) => trial.taskId === task.taskId && trial.attempt === attempt);
    rows.push({...task, attempt, ...observed, key: `${task.taskId}:${attempt}`});
  }
  if (!rows.some((row) => row.key === selected)) selected = rows.length ? rows[0].key : null;
  const totalPages = Math.max(1, Math.ceil(rows.length / pageSize));
  page = Math.min(page, totalPages - 1);
  byId("pages").replaceChildren();
  for (const [label, destination] of [["Previous", page - 1], [`${page + 1} / ${totalPages}`, page], ["Next", page + 1]]) {
    const button = node("button", label);
    button.disabled = destination < 0 || destination >= totalPages || destination === page;
    if (destination === page) button.setAttribute("aria-current", "page");
    button.onclick = () => { page = destination; render(current); };
    byId("pages").append(button);
  }
  byId("tasks").replaceChildren();
  for (const row of rows.slice(page * pageSize, (page + 1) * pageSize)) {
    const tr = node("tr");
    tr.dataset.selected = String(row.key === selected);
    const taskCell = node("td");
    const button = node("button", `${row.taskId} / ${row.attempt}`, "task-button");
    button.onclick = () => { selected = row.key; render(current); };
    taskCell.append(button);
    tr.append(taskCell);
    for (const key of ["phase", "agentStatus", "verifierStatus", "reward"]) tr.append(node("td", display(row[key])));
    byId("tasks").append(tr);
  }
  byId("empty").hidden = rows.length > 0;
  renderDetails(state, rows);
}
async function poll() {
  try {
    const response = await fetch("/api/state", {cache: "no-store"});
    if (!response.ok) throw new Error("Monitor unavailable");
    connected = true;
    render(await response.json());
  } catch (_) {
    connected = false;
    byId("connection").textContent = "Monitor disconnected / job status unknown";
    byId("warning").hidden = false;
    byId("warning").textContent = "Cannot read observations. This does not stop the job or establish task failure.";
  } finally { window.setTimeout(poll, 2000); }
}
window.setInterval(() => {
  for (const view of slotViews.values()) {
    // Freeze running clocks on disconnect or stale lifecycle observations.
    if (view.row && (view.row.finishedAt || (connected && current && current.monitorStatus==='live'))) view.timer.textContent=elapsed(view.row);
  }
}, 1000);
poll();
