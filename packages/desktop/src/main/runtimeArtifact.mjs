import path from "node:path";

export const DESKTOP_RUNTIME_TARGET = "x86_64-pc-windows-msvc";

export const runtimeExeName = (platform = process.platform) =>
  platform === "win32" ? "centaeris-runtime.exe" : "centaeris-runtime";

export const runtimeArtifactPath = (
  repoRoot,
  profile,
  platform = process.platform,
  targetDirectory = process.env.CARGO_TARGET_DIR,
) => path.join(path.resolve(repoRoot, targetDirectory || "target"), profile, runtimeExeName(platform));
