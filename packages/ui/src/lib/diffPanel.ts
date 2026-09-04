export type DiffPanelFile = {
  id: string;
  path: string;
  title: string;
  diffPreview: string;
  added: number;
  removed: number;
  diffAvailable?: boolean;
  diffUnavailableReason?: string;
  taskId?: string;
  taskTitle?: string;
};

export type DiffPanelData = {
  id: string;
  title: string;
  subtitle: string;
  files: DiffPanelFile[];
  totalAdded: number;
  totalRemoved: number;
  sourceMessageId?: string;
};

export const countDiffPreviewChanges = (
  diffPreview: string | undefined,
): { added: number; removed: number } => {
  const lines = String(diffPreview || "").split(/\r?\n/);
  let added = 0;
  let removed = 0;
  for (const line of lines) {
    if (line.startsWith("+++") || line.startsWith("---")) {
      continue;
    }
    if (line.startsWith("+")) {
      added += 1;
    } else if (line.startsWith("-")) {
      removed += 1;
    }
  }
  return { added, removed };
};

export const diffPanelFileTitle = (path: string): string => {
  const normalized = path.trim().replace(/\\/g, "/");
  return normalized.split("/").filter(Boolean).at(-1) || normalized || "changes";
};
