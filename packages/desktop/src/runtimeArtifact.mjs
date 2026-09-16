import path from "node:path";
export const DESKTOP_RUNTIME_TARGET = "x86_64-unknown-linux-gnu";

export const runtimeArtifactPath = (repoRoot, profile, platform = process.platform) =>
  path.join(repoRoot, "target", ...(platform === "win32" ? ["wsl"] : []), profile, "centaeris-runtime");
