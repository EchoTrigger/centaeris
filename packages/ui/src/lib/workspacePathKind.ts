const FILE_EXTENSION_PATTERN = /(?:^|.)\.[A-Za-z0-9]{1,16}$/;

export const isWorkspaceDirectoryLikePath = (path: string): boolean => {
  const normalized = path.trim().replace(/\\/g, "/");
  if (!normalized) {
    return false;
  }
  if (normalized.endsWith("/")) {
    return true;
  }
  if (!normalized.includes("/")) {
    return false;
  }
  const lastSegment = normalized.split("/").filter(Boolean).at(-1) ?? "";
  if (!lastSegment || lastSegment === "." || lastSegment === "..") {
    return false;
  }
  return !FILE_EXTENSION_PATTERN.test(lastSegment);
};
