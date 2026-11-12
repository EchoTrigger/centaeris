import { createContext, useContext, useState } from "react";

export const workStatuses = ["待开始", "进行中", "已完成", "已取消"];
export const exampleProjects = [
  { id: "knowledge", name: "知识库体验改进", icon: "K", owner: "我", description: "让团队更快找到可信的依据，并能回到原文核验。", outcome: "形成一份检索体验调研与改进建议。", materials: ["检索体验访谈.md", "资料检索要求.md"], skill: "证据检索" },
  { id: "release", name: "开放工具包发布", icon: "R", owner: "陈悦", description: "整理公开文档与示例，准备可以被其他团队复用的工具包。", outcome: "交付完整的发布说明、示例材料与检查报告。", materials: ["发布材料清单.md", "工具包使用指南.md"], skill: "材料核查" },
];
const initialTasks = () => [
  { id: "CW-12", title: "核查工具包发布材料", project: "release", status: "进行中", owner: "我", priority: "高", label: "材料核查", description: "按照发布材料清单检查文档与附件，记录缺失内容和版本冲突，补充后继续复核。", deliverable: "一份有证据引用的材料核查报告", attention: true, run: "等待补充材料", checks: [{ id: "guide", title: "使用指南与版本一致", result: "通过", evidence: "工具包使用指南.md · 第 2 节" }, { id: "example", title: "包含可运行示例", result: "未通过", evidence: "发布材料清单.md · 第 3 项", issue: "缺少快速开始示例", resolution: "待处理" }, { id: "license", title: "说明开源许可", result: "通过", evidence: "工具包使用指南.md · 许可说明" }], events: ["核查助手检查了 3 项要求，发现 1 项缺失。"], conversations: [{ title: "发布材料首轮核查", text: "已完成当前清单的检查。使用指南和许可说明齐全，快速开始示例尚未提供。", state: "本轮执行已结束" }] },
  { id: "CW-11", title: "梳理资料检索的权限边界", project: "knowledge", status: "进行中", owner: "我", priority: "高", label: "证据检索", description: "查找当前资料中的访问要求，区分原文、搜索片段与派生成果的可见范围。", deliverable: "一份附原文定位的权限边界说明", attention: true, run: "等待确认范围", checks: [], events: ["研究助手整理了两条依据，等待确认讨论范围。"], conversations: [{ title: "检索与证据整理", text: "两份示例资料均提到：搜索结果只包含当前可访问内容。建议同时核查摘要与关联附件的可见范围。", state: "本轮执行已结束" }] },
  { id: "CW-10", title: "整理访谈中的高频查询", project: "knowledge", status: "进行中", owner: "陈悦", priority: "中", label: "研究", description: "整理访谈中的查询意图，保留原始表达与资料引用。", deliverable: "高频问题清单", attention: false, run: "尚未委托", checks: [], events: [], conversations: [] },
  { id: "CW-9", title: "撰写工具包发布说明", project: "release", status: "待开始", owner: "我", priority: "中", label: "写作", description: "概述工具包用途、安装方法和示例。", deliverable: "发布说明初稿", attention: false, run: "尚未委托", checks: [], events: [], conversations: [] },
  { id: "CW-8", title: "整理本周的研究笔记", project: "", status: "待开始", owner: "我", priority: "低", label: "研究", description: "整理值得继续研究的问题。", deliverable: "研究笔记", attention: false, run: "尚未委托", checks: [], events: [], conversations: [] },
  { id: "CW-7", title: "汇总资料来源", project: "knowledge", status: "已完成", owner: "我", priority: "中", label: "研究", description: "记录本次研究采用的资料来源。", deliverable: "资料来源目录", attention: false, run: "执行已完成", checks: [], events: ["资料来源目录已确认。"], conversations: [] },
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
    setTasks(items => [{ ...values, id, status: "待开始", owner: "我", priority: "中", label: "", attention: false, run: "尚未委托", checks: [], events: ["创建了任务。"], conversations: [] }, ...items]);
    return id;
  }
  return <WorkContext.Provider value={{ tasks, favorites, updateTask, createTask, toggleFavorite: id => setFavorites(items => items.includes(id) ? items.filter(item => item !== id) : [...items, id]), reset: () => { setTasks(initialTasks()); setFavorites(["knowledge"]); } }}>{children}</WorkContext.Provider>;
}
export function useWorkPrototype() { return useContext(WorkContext); }
