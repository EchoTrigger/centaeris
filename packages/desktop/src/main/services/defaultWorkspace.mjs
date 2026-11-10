import fs from "node:fs/promises";
import path from "node:path";

export const ensureDefaultWorkspaceDirectory = async (
  homePath,
  now = new Date(),
) => {
  const home = typeof homePath === "string" ? homePath.trim() : "";
  if (!home || !path.isAbsolute(home)) {
    throw new Error("default workspace home path must be absolute");
  }
  if (!(now instanceof Date) || Number.isNaN(now.getTime())) {
    throw new Error("default workspace date is invalid");
  }
  const date = [
    now.getFullYear(),
    String(now.getMonth() + 1).padStart(2, "0"),
    String(now.getDate()).padStart(2, "0"),
  ].join("");
  const root = path.join(home, `centaeris-cwd-${date}`);
  await fs.mkdir(root, { recursive: true });
  return root;
};
