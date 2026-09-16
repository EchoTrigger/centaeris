import path from "node:path";

export const requireDistribution = (value) => {
  if (typeof value !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(value)) {
    throw new Error("WSL distribution name is invalid");
  }
  return value;
};

export const toLinuxPath = (value, distribution, { importSource = false } = {}) => {
  requireDistribution(distribution);
  if (typeof value !== "string" || !value || value.includes("\0")) {
    throw new Error("WSL path is invalid");
  }
  if (value.startsWith("\\\\?\\UNC\\")) value = "\\\\" + value.slice(8);
  else if (value.startsWith("\\\\?\\")) value = value.slice(4);
  const unc = /^[/\\]{2}(?:wsl\.localhost|wsl\$)[/\\]([^/\\]+)([/\\].*)?$/i.exec(value);
  let mapped;
  if (unc) {
    if (unc[1].toLowerCase() !== distribution.toLowerCase()) {
      throw new Error(`Path belongs to another WSL distribution: ${unc[1]}`);
    }
    mapped = (unc[2] || "/").replaceAll("\\", "/");
  } else if (/^[A-Za-z]:[/\\]/.test(value)) {
    mapped = `/mnt/${value[0].toLowerCase()}${value.slice(2).replaceAll("\\", "/")}`;
  } else if (value.startsWith("/") && !value.startsWith("//")) {
    mapped = value;
  } else {
    throw new Error("An absolute Linux or selected WSL distribution path is required");
  }
  mapped = path.posix.normalize(mapped);
  if (!importSource && /^\/mnt\/[a-z](?:\/|$)/i.test(mapped)) {
    throw new Error("Execution resources must be on the WSL Linux filesystem; select a folder under \\\\wsl.localhost\\" + distribution);
  }
  return mapped;
};

export const toWindowsPath = (value, distribution) => {
  const mapped = toLinuxPath(value, distribution, { importSource: true });
  const drive = /^\/mnt\/([a-z])(?:\/(.*))?$/i.exec(mapped);
  if (drive) return `${drive[1].toUpperCase()}:\\${(drive[2] || "").replaceAll("/", "\\")}`;
  return `\\\\wsl.localhost\\${distribution}${mapped.replaceAll("/", "\\")}`;
};
