export function officeFileType(filename = "") {
  const extension = filename.split(".").pop().toLowerCase();
  return ["docx", "xlsx", "pptx"].includes(extension) && filename.includes(".") ? extension : null;
}
