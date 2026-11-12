import { createContext, useContext, useState } from "react";

// Store translation references for sample copy, while user-authored strings stay verbatim.
export function workText(key, values) { return { key, values }; }
export function displayWorkText(value, t) {
  if (typeof value === "string") return value;
  return t(value.key, Object.fromEntries(Object.entries(value.values || {}).map(([key, text]) => [key, typeof text === "object" && text !== null ? displayWorkText(text, t) : text])));
}

export const workStatuses = ["todo", "in_progress", "done", "cancelled"];
export const exampleProjects = [
  { id: "knowledge", name: workText("work.example.knowledge.name"), icon: "K", owner: "me", description: workText("work.example.knowledge.description"), outcome: workText("work.example.knowledge.outcome"), materials: [workText("work.example.document.interviews"), workText("work.example.document.requirements")], skill: workText("work.label.evidence") },
  { id: "release", name: workText("work.example.release.name"), icon: "R", owner: "chen_yue", description: workText("work.example.release.description"), outcome: workText("work.example.release.outcome"), materials: [workText("work.example.document.checklist"), workText("work.example.document.guide")], skill: workText("work.label.review") },
];
const initialTasks = () => [
  { id: "CW-12", title: workText("work.example.review.title"), project: "release", status: "in_progress", owner: "me", priority: "high", label: workText("work.label.review"), description: workText("work.example.review.description"), deliverable: workText("work.example.review.deliverable"), attention: true, run: workText("work.run.awaitingMaterials"), checks: [{ id: "guide", title: workText("work.example.check.guide"), result: "passed", evidence: workText("work.example.check.guideEvidence") }, { id: "example", title: workText("work.example.check.example"), result: "failed", evidence: workText("work.example.check.exampleEvidence"), issue: workText("work.example.check.missingExample"), resolution: "pending" }, { id: "license", title: workText("work.example.check.license"), result: "passed", evidence: workText("work.example.check.licenseEvidence") }], events: [workText("work.example.review.event")], conversations: [{ title: workText("work.example.review.conversation"), text: workText("work.example.review.summary"), state: workText("work.run.finished") }] },
  { id: "CW-11", title: workText("work.example.permissions.title"), project: "knowledge", status: "in_progress", owner: "me", priority: "high", label: workText("work.label.evidence"), description: workText("work.example.permissions.description"), deliverable: workText("work.example.permissions.deliverable"), attention: true, run: workText("work.run.awaitingScope"), checks: [], events: [workText("work.example.permissions.event")], conversations: [{ title: workText("work.example.permissions.conversation"), text: workText("work.example.permissions.summary"), state: workText("work.run.finished") }] },
  { id: "CW-10", title: workText("work.example.queries.title"), project: "knowledge", status: "in_progress", owner: "chen_yue", priority: "medium", label: workText("work.label.research"), description: workText("work.example.queries.description"), deliverable: workText("work.example.queries.deliverable"), attention: false, run: workText("work.run.unassigned"), checks: [], events: [], conversations: [] },
  { id: "CW-9", title: workText("work.example.releaseNotes.title"), project: "release", status: "todo", owner: "me", priority: "medium", label: workText("work.label.writing"), description: workText("work.example.releaseNotes.description"), deliverable: workText("work.example.releaseNotes.deliverable"), attention: false, run: workText("work.run.unassigned"), checks: [], events: [], conversations: [] },
  { id: "CW-8", title: workText("work.example.notes.title"), project: "", status: "todo", owner: "me", priority: "low", label: workText("work.label.research"), description: workText("work.example.notes.description"), deliverable: workText("work.example.notes.deliverable"), attention: false, run: workText("work.run.unassigned"), checks: [], events: [], conversations: [] },
  { id: "CW-7", title: workText("work.example.sources.title"), project: "knowledge", status: "done", owner: "me", priority: "medium", label: workText("work.label.research"), description: workText("work.example.sources.description"), deliverable: workText("work.example.sources.deliverable"), attention: false, run: workText("work.run.complete"), checks: [], events: [workText("work.example.sources.event")], conversations: [] },
];
const WorkContext = createContext(null);
export function WorkPrototypeProvider({ children }) {
  const [tasks, setTasks] = useState(initialTasks);
  const [favorites, setFavorites] = useState(["knowledge"]);
  function updateTask(id, change, event) {
    setTasks(items => items.map(item => item.id === id ? { ...item, ...change, events: event ? [...item.events, event] : item.events } : item));
  }
  function createTask(values) {
    const id = `CW-${Math.max(...tasks.map(item => Number(item.id.slice(3)))) + 1}`;
    setTasks(items => [{ ...values, id, status: "todo", owner: "me", priority: "medium", label: "", attention: false, run: workText("work.run.unassigned"), checks: [], events: [workText("work.event.created")], conversations: [] }, ...items]);
    return id;
  }
  return <WorkContext.Provider value={{ tasks, favorites, updateTask, createTask, toggleFavorite: id => setFavorites(items => items.includes(id) ? items.filter(item => item !== id) : [...items, id]), reset: () => { setTasks(initialTasks()); setFavorites(["knowledge"]); } }}>{children}</WorkContext.Provider>;
}
export function useWorkPrototype() { return useContext(WorkContext); }
