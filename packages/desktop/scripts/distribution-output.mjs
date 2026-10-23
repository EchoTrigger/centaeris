import fs from "node:fs/promises";
import path from "node:path";

export async function reserveDistributionOutput(directory) {
  if (!path.isAbsolute(directory)) throw new Error("Desktop distribution output must be absolute");
  try {
    await fs.mkdir(directory);
  } catch (error) {
    if (error.code === "EEXIST") throw new Error("Desktop distribution output already exists; choose a fresh directory", { cause: error });
    throw error;
  }
  return directory;
}
