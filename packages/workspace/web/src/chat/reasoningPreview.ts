export function reasoningPreview(text: string): string {
  const plain = text.replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/^[\s>#*-]+/gm, "").replace(/[`*_~]/g, "").replace(/\s+/g, " ").trim();
  return Array.from(plain).slice(-180).join("");
}
