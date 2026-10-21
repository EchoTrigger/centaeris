import type { ComponentProps } from "react";
import { vi } from "vitest";
import { Sidebar } from "../../src/components/Sidebar";
export const sidebarProps = (
	overrides: Partial<ComponentProps<typeof Sidebar>> = {},
): ComponentProps<typeof Sidebar> => ({
	sessions: [
		{
			id: "s",
			title: "Original",
			cwd: "D:/test",
			sessionKind: "main",
			messageCount: 1,
		},
	],
	currentSessionId: "s",
	workspaces: [{ root: "D:/test", name: "test", sortOrder: 0, updatedAt: 0 }],
	activeWorkspaceRoot: "D:/test",
	runningSessionIds: new Set(),
	completedSessionIds: new Set(),
	workspaceCatalogError: null,
	onNewChat: vi.fn(),
	onOpenWorkspace: vi.fn(),
	onSelectWorkspace: vi.fn(),
	onRetryWorkspaceCatalog: vi.fn(async () => {}),
	onResetWorkspaceCatalog: vi.fn(async () => {}),
	onSelectSession: vi.fn(),
	onRenameSession: vi.fn(async () => {}),
	onDeleteSession: vi.fn(async () => {}),
	onOpenResource: vi.fn(),
	onOpenFile: vi.fn(),
	...overrides,
});
