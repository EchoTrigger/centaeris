// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, Outlet, RouterProvider } from "react-router";
import { afterEach, expect, test } from "vitest";
import WorkPrototypeRoute from "../../src/work/WorkPrototypeRoute";
import { WorkPrototypeProvider } from "../../src/work/workPrototype";

afterEach(cleanup);
function mount(path = "/w/demo/work") {
  const router = createMemoryRouter([{ id: "workspace", path: "/w/demo", loader: () => ({ workspace: { id: "demo" } }), HydrateFallback: () => null,
    Component: () => <WorkPrototypeProvider><Outlet /></WorkPrototypeProvider>,
    children: [{ path: "work/*", Component: WorkPrototypeRoute }],
  }], { initialEntries: [path] });
  render(<RouterProvider router={router} />);
  return router;
}
test("material correction records evidence progress without completing the business task", async () => {
  const user = userEvent.setup();
  const router = mount();
  await user.click(await screen.findByRole("link", { name: /核查工具包发布材料/ }));
  await user.click(screen.getByRole("button", { name: "核查结果" }));
  await user.click(screen.getByRole("button", { name: "补充示例文件" }));
  expect(screen.getByText("待复核")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "模拟复核" }));
  expect(screen.getByText("3 / 3 项通过")).toBeTruthy();
  expect((screen.getByLabelText("状态") as HTMLSelectElement).value).toBe("进行中");
  await user.selectOptions(screen.getByLabelText("状态"), "已完成");
  await router.navigate("/w/demo/work?tab=completed");
  await user.click(await screen.findByRole("link", { name: /核查工具包发布材料/ }));
  await user.click(screen.getByRole("button", { name: "活动" }));
  expect(screen.getByText("补充了示例文件 quick-start.md。")).toBeTruthy();
  expect(within(screen.getByRole("list")).getByText("状态变更为已完成。")).toBeTruthy();
});

test("new tasks belong to the chosen project and remain available across views", async () => {
  const user = userEvent.setup();
  const router = mount("/w/demo/work/projects/knowledge?tab=tasks");
  await user.click(await screen.findByRole("button", { name: "新建任务" }));
  const dialog = within(screen.getByRole("dialog", { name: "新建任务" }));
  await user.type(dialog.getByLabelText("任务名称"), "整理研究结论");
  await user.type(dialog.getByLabelText("说明"), "保留引用与待确认事项");
  await user.click(dialog.getByRole("button", { name: "创建任务" }));
  expect(await screen.findByRole("heading", { name: "整理研究结论" })).toBeTruthy();
  expect((screen.getByLabelText("项目") as HTMLSelectElement).value).toBe("knowledge");
  await user.click(screen.getByRole("button", { name: "模拟一次执行" }));
  expect((screen.getByLabelText("状态") as HTMLSelectElement).value).toBe("待开始");
  await router.navigate("/w/demo/work/projects/knowledge?tab=tasks");
  await user.click(await screen.findByRole("link", { name: /整理研究结论/ }));
  await user.click(screen.getByRole("button", { name: "会话 1" }));
  await user.click(screen.getByRole("button", { name: /任务执行记录/ }));
  expect(screen.getByRole("dialog", { name: "任务执行记录" })).toBeTruthy();
  await user.keyboard("{Escape}");
  expect(screen.queryByRole("dialog")).toBeNull();
});
